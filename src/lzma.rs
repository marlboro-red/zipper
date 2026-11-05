use std::io::{self, Read};

const PROBABILITY_BITS: u32 = 11;
const PROBABILITY_ONE: u16 = 1 << PROBABILITY_BITS;
const MOVE_BITS: u32 = 5;
const TOP: u32 = 1 << 24;
const STATES: usize = 12;
const POSITION_STATES_MAX: usize = 1 << 4;
const LENGTH_STATES: usize = 4;
const MATCH_LENGTH_MIN: usize = 2;
const DISTANCE_SLOTS: usize = 64;
const END_POSITION_MODEL: usize = 14;
const FULL_DISTANCES: usize = 1 << (END_POSITION_MODEL / 2);
const ALIGN_BITS: u32 = 4;
const INPUT_CHUNK: usize = 64 * 1024;
const SYMBOL_INPUT_MAX: usize = 64;
const DICTIONARY_MIN: usize = 4096;
const OUTPUT_CHUNK: usize = 64 * 1024;
const COPY_CHUNK: usize = 16;

fn data_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Properties {
    pub(crate) lc: u32,
    pub(crate) lp: u32,
    pub(crate) pb: u32,
}

impl Properties {
    // SPEC(lzma-specification.txt LZMA properties): one byte holds (pb * 5 + lp) * 9 + lc.
    pub(crate) fn from_byte(byte: u8) -> Option<Self> {
        let byte = u32::from(byte);
        (byte < 9 * 5 * 5).then_some(Properties {
            lc: byte % 9,
            lp: byte / 9 % 5,
            pb: byte / 45,
        })
    }
}

struct RangeDecoder<R> {
    input: R,
    buffer: Vec<u8>,
    start: usize,
    end: usize,
    ended: bool,
    taken: u64,
    input_limit: u64,
    range: u32,
    code: u32,
}

impl<R: Read> RangeDecoder<R> {
    fn new(input: R) -> Self {
        RangeDecoder {
            input,
            buffer: vec![0; INPUT_CHUNK + SYMBOL_INPUT_MAX],
            start: 0,
            end: 0,
            ended: false,
            taken: 0,
            input_limit: u64::MAX,
            range: u32::MAX,
            code: 0,
        }
    }

    fn refill(&mut self) -> io::Result<()> {
        self.buffer.copy_within(self.start..self.end, 0);
        self.end -= self.start;
        self.start = 0;
        while self.end < INPUT_CHUNK {
            let read = self.input.read(&mut self.buffer[self.end..INPUT_CHUNK])?;
            if read == 0 {
                self.ended = true;
                break;
            }
            self.end += read;
            self.taken += read as u64;
        }
        self.buffer[self.end..].fill(0);
        Ok(())
    }

    fn consumed(&self) -> u64 {
        self.taken - (self.end - self.start) as u64
    }

    fn limited_end(&self) -> Option<usize> {
        let buffer_start = self.taken - self.end as u64;
        let limit = self.input_limit.saturating_sub(buffer_start);
        (limit <= self.end as u64).then_some(limit as usize)
    }

    fn complete(&self) -> bool {
        self.ended || self.limited_end().is_some()
    }

    fn bits(&self) -> Bits<'_> {
        let end = self.limited_end().unwrap_or(self.end);
        Bits {
            input: &self.buffer,
            position: self.start,
            end,
            room: if self.complete() {
                end + SYMBOL_INPUT_MAX
            } else {
                end
            },
            range: self.range,
            code: self.code,
        }
    }

    fn fill_to(&mut self, count: usize) -> io::Result<()> {
        if self.end - self.start < count && !self.ended {
            self.refill()?;
        }
        Ok(())
    }

    fn next_byte(&mut self) -> io::Result<Option<u8>> {
        self.fill_to(1)?;
        if self.start == self.end {
            return Ok(None);
        }
        self.start += 1;
        Ok(Some(self.buffer[self.start - 1]))
    }

    fn copy_into(&mut self, window: &mut Window, count: usize) -> io::Result<usize> {
        self.fill_to(1)?;
        let run = count.min(self.end - self.start);
        if run == 0 {
            return Err(data_error("truncated LZMA2 stream"));
        }
        window.write(&self.buffer[self.start..self.start + run]);
        self.start += run;
        Ok(run)
    }

    fn keep(&mut self, (position, range, code): (usize, u32, u32)) {
        self.start = position;
        self.range = range;
        self.code = code;
    }

    fn start(&mut self) -> io::Result<()> {
        self.fill_to(5)?;
        let mut bits = self.bits();
        if bits.byte() != 0 {
            return Err(data_error("LZMA stream does not start with a zero byte"));
        }
        bits.range = u32::MAX;
        bits.code = 0;
        for _ in 0..4 {
            bits.code = bits.code << 8 | u32::from(bits.byte());
        }
        bits.ran_past_input()?;
        if bits.code == bits.range {
            return Err(data_error("LZMA range coder starts out of range"));
        }
        let kept = bits.parts();
        self.keep(kept);
        Ok(())
    }
}

struct Bits<'a> {
    input: &'a [u8],
    position: usize,
    end: usize,
    room: usize,
    range: u32,
    code: u32,
}

impl Bits<'_> {
    #[inline(always)]
    fn needs_input(&self) -> bool {
        self.position + SYMBOL_INPUT_MAX > self.room
    }

    fn ran_past_input(&self) -> io::Result<()> {
        if self.position > self.end {
            return Err(data_error("truncated LZMA stream"));
        }
        Ok(())
    }

    fn parts(&self) -> (usize, u32, u32) {
        (self.position, self.range, self.code)
    }

    #[inline(always)]
    fn byte(&mut self) -> u8 {
        let byte = self.input[self.position];
        self.position += 1;
        byte
    }

    #[inline(always)]
    fn normalize(&mut self) {
        if self.range < TOP {
            self.range <<= 8;
            self.code = self.code << 8 | u32::from(self.byte());
        }
    }

    #[inline(always)]
    fn bit(&mut self, probability: &mut u16) -> u32 {
        let bound = (self.range >> PROBABILITY_BITS) * u32::from(*probability);
        let bit = u32::from(self.code >= bound);
        let mask = 0u32.wrapping_sub(bit);
        self.range = (bound & !mask) | (self.range.wrapping_sub(bound) & mask);
        self.code -= bound & mask;
        let toward_zero = *probability >> MOVE_BITS;
        let toward_one = (PROBABILITY_ONE - *probability) >> MOVE_BITS;
        *probability = probability
            .wrapping_add(toward_one & !(mask as u16))
            .wrapping_sub(toward_zero & mask as u16);
        self.normalize();
        bit
    }

    #[inline(always)]
    fn branching_bit(&mut self, probability: &mut u16) -> u32 {
        let bound = (self.range >> PROBABILITY_BITS) * u32::from(*probability);
        let bit = if self.code < bound {
            self.range = bound;
            *probability += (PROBABILITY_ONE - *probability) >> MOVE_BITS;
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            *probability -= *probability >> MOVE_BITS;
            1
        };
        self.normalize();
        bit
    }

    fn direct_bits(&mut self, count: u32) -> u32 {
        let mut value = 0u32;
        for _ in 0..count {
            self.range >>= 1;
            let bit = u32::from(self.code >= self.range);
            if bit == 1 {
                self.code -= self.range;
            }
            value = value << 1 | bit;
            self.normalize();
        }
        value
    }

    #[inline(always)]
    fn tree(&mut self, probabilities: &mut [u16], bits: u32) -> u32 {
        let mut index = 1usize;
        for _ in 0..bits {
            index = index << 1 | self.bit(&mut probabilities[index]) as usize;
        }
        index as u32 - (1 << bits)
    }

    #[inline(always)]
    fn reverse_tree(&mut self, probabilities: &mut [u16], bits: u32) -> u32 {
        let mut index = 1usize;
        let mut value = 0u32;
        for bit_index in 0..bits {
            let bit = self.bit(&mut probabilities[index]);
            index = index << 1 | bit as usize;
            value |= bit << bit_index;
        }
        value
    }

    fn finished_cleanly(&self) -> bool {
        self.code == 0
    }
}

struct LengthDecoder {
    choice: u16,
    choice2: u16,
    low: [[u16; 8]; POSITION_STATES_MAX],
    middle: [[u16; 8]; POSITION_STATES_MAX],
    high: [u16; 256],
}

impl LengthDecoder {
    fn new() -> Self {
        LengthDecoder {
            choice: PROBABILITY_ONE / 2,
            choice2: PROBABILITY_ONE / 2,
            low: [[PROBABILITY_ONE / 2; 8]; POSITION_STATES_MAX],
            middle: [[PROBABILITY_ONE / 2; 8]; POSITION_STATES_MAX],
            high: [PROBABILITY_ONE / 2; 256],
        }
    }

    #[inline(always)]
    fn decode(&mut self, range: &mut Bits, position_state: usize) -> usize {
        if range.branching_bit(&mut self.choice) == 0 {
            return range.tree(&mut self.low[position_state], 3) as usize;
        }
        if range.branching_bit(&mut self.choice2) == 0 {
            return 8 + range.tree(&mut self.middle[position_state], 3) as usize;
        }
        16 + range.tree(&mut self.high, 8) as usize
    }
}

struct Window {
    buffer: Vec<u8>,
    size: usize,
    position: usize,
    full: bool,
    total: u64,
    last: u8,
}

impl Window {
    fn new(size: usize) -> Self {
        Window {
            buffer: vec![0; size.max(1) + 2 * COPY_CHUNK],
            size: size.max(1) + COPY_CHUNK,
            position: 0,
            full: false,
            total: 0,
            last: 0,
        }
    }

    fn reset(&mut self) {
        self.position = 0;
        self.full = false;
        self.total = 0;
        self.last = 0;
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut left = bytes;
        while !left.is_empty() {
            let run = left.len().min(self.size - self.position);
            self.buffer[self.position..self.position + run].copy_from_slice(&left[..run]);
            self.position += run;
            self.total += run as u64;
            left = &left[run..];
            if self.position == self.size {
                self.position = 0;
                self.full = true;
            }
        }
        if let Some(&last) = bytes.last() {
            self.last = last;
        }
    }

    #[inline(always)]
    fn put(&mut self, byte: u8) {
        self.buffer[self.position] = byte;
        self.last = byte;
        self.position += 1;
        self.total += 1;
        if self.position == self.size {
            self.position = 0;
            self.full = true;
        }
    }

    #[inline(always)]
    fn back(&self, distance: usize) -> u8 {
        let index = if distance <= self.position {
            self.position - distance
        } else {
            self.size + self.position - distance
        };
        self.buffer[index]
    }

    fn reaches(&self, distance: usize) -> bool {
        distance <= self.position || (self.full && distance <= self.size - COPY_CHUNK)
    }

    #[inline(always)]
    fn copy_in_chunks(&mut self, source: usize, run: usize) {
        let mut done = 0;
        while done < run {
            let from = source + done;
            let to = self.position + done;
            let chunk: [u8; COPY_CHUNK] = self.buffer[from..from + COPY_CHUNK].try_into().unwrap();
            self.buffer[to..to + COPY_CHUNK].copy_from_slice(&chunk);
            done += COPY_CHUNK;
        }
    }

    fn copy(&mut self, distance: usize, length: usize) {
        let size = self.size;
        let mut left = length;
        while left > 0 {
            let source = if distance <= self.position {
                self.position - distance
            } else {
                size + self.position - distance
            };
            let run = left.min(size - self.position).min(size - source);
            let run = if distance >= COPY_CHUNK {
                self.copy_in_chunks(source, run);
                run
            } else {
                let run = run.min(distance);
                self.buffer.copy_within(source..source + run, self.position);
                run
            };
            self.position += run;
            self.last = self.buffer[self.position - 1];
            self.total += run as u64;
            left -= run;
            if self.position == size {
                self.position = 0;
                self.full = true;
            }
        }
    }
}

pub(crate) struct ChunkState {
    properties: Properties,
    literal: Vec<u16>,
    is_match: [[u16; POSITION_STATES_MAX]; STATES],
    is_rep: [u16; STATES],
    is_rep0: [u16; STATES],
    is_rep1: [u16; STATES],
    is_rep2: [u16; STATES],
    is_rep0_long: [[u16; POSITION_STATES_MAX]; STATES],
    distance_slot: [[u16; DISTANCE_SLOTS]; LENGTH_STATES],
    distance_special: [u16; 1 + FULL_DISTANCES - END_POSITION_MODEL],
    align: [u16; 1 << ALIGN_BITS],
    length: LengthDecoder,
    rep_length: LengthDecoder,
    state: usize,
    reps: [usize; 4],
    dictionary: usize,
}

impl ChunkState {
    pub(crate) fn new(properties: Properties, dictionary: usize) -> Self {
        let mut state = ChunkState {
            properties,
            literal: Vec::new(),
            is_match: [[0; POSITION_STATES_MAX]; STATES],
            is_rep: [0; STATES],
            is_rep0: [0; STATES],
            is_rep1: [0; STATES],
            is_rep2: [0; STATES],
            is_rep0_long: [[0; POSITION_STATES_MAX]; STATES],
            distance_slot: [[0; DISTANCE_SLOTS]; LENGTH_STATES],
            distance_special: [0; 1 + FULL_DISTANCES - END_POSITION_MODEL],
            align: [0; 1 << ALIGN_BITS],
            length: LengthDecoder::new(),
            rep_length: LengthDecoder::new(),
            state: 0,
            reps: [0; 4],
            dictionary: dictionary.max(DICTIONARY_MIN),
        };
        state.reset(properties);
        state
    }

    pub(crate) fn reset(&mut self, properties: Properties) {
        let half = PROBABILITY_ONE / 2;
        self.properties = properties;
        self.literal.clear();
        self.literal
            .resize(0x300 << (properties.lc + properties.lp), half);
        for row in &mut self.is_match {
            row.fill(half);
        }
        for row in &mut self.is_rep0_long {
            row.fill(half);
        }
        for table in [
            &mut self.is_rep,
            &mut self.is_rep0,
            &mut self.is_rep1,
            &mut self.is_rep2,
        ] {
            table.fill(half);
        }
        for row in &mut self.distance_slot {
            row.fill(half);
        }
        self.distance_special.fill(half);
        self.align.fill(half);
        self.length = LengthDecoder::new();
        self.rep_length = LengthDecoder::new();
        self.state = 0;
        self.reps = [0; 4];
    }

    fn literal_state_after(state: usize) -> usize {
        match state {
            0..=3 => 0,
            4..=9 => state - 3,
            _ => state - 6,
        }
    }

    fn decode_literal(&mut self, range: &mut Bits, window: &mut Window) {
        let Properties { lc, lp, .. } = self.properties;
        let previous = window.last;
        let position = window.total as usize;
        let context = ((position & ((1 << lp) - 1)) << lc) + (usize::from(previous) >> (8 - lc));
        let probabilities = &mut self.literal[0x300 * context..0x300 * (context + 1)];
        let mut symbol = 1usize;
        if self.state >= 7 {
            let mut matched = usize::from(window.back(self.reps[0] + 1));
            let mut offset = 0x100usize;
            while symbol < 0x100 {
                matched <<= 1;
                let match_bit = matched & offset;
                let bit = range.bit(&mut probabilities[offset + match_bit + symbol]) as usize;
                symbol = symbol << 1 | bit;
                offset &= match_bit ^ bit.wrapping_sub(1);
            }
        } else {
            while symbol < 0x100 {
                symbol = symbol << 1 | range.bit(&mut probabilities[symbol]) as usize;
            }
        }
        window.put(symbol as u8);
        self.state = Self::literal_state_after(self.state);
    }

    fn decode_distance(&mut self, range: &mut Bits, length: usize) -> usize {
        let length_state = length.min(LENGTH_STATES - 1);
        let slot = range.tree(&mut self.distance_slot[length_state], 6) as usize;
        if slot < 4 {
            return slot;
        }
        let direct = (slot >> 1) as u32 - 1;
        let mut distance = (2 | (slot & 1)) << direct;
        if slot < END_POSITION_MODEL {
            let base = distance - slot;
            distance += range.reverse_tree(&mut self.distance_special[base..], direct) as usize;
        } else {
            distance += (range.direct_bits(direct - ALIGN_BITS) as usize) << ALIGN_BITS;
            distance += range.reverse_tree(&mut self.align, ALIGN_BITS) as usize;
        }
        distance
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Stop {
    Limit,
    EndMarker,
}

fn decode<R: Read>(
    state: &mut ChunkState,
    range: &mut RangeDecoder<R>,
    window: &mut Window,
    limit: u64,
    pending: &mut usize,
) -> io::Result<Stop> {
    loop {
        let mut bits = range.bits();
        let stop = decode_symbols(state, &mut bits, window, limit, pending);
        let kept = bits.parts();
        range.keep(kept);
        match stop? {
            Some(stop) => return Ok(stop),
            None if range.complete() => return Err(data_error("truncated LZMA stream")),
            None => range.refill()?,
        }
    }
}

fn decode_symbols(
    state: &mut ChunkState,
    range: &mut Bits,
    window: &mut Window,
    limit: u64,
    pending: &mut usize,
) -> io::Result<Option<Stop>> {
    let position_mask = (1usize << state.properties.pb) - 1;
    while window.total < limit {
        if *pending > 0 {
            let distance = state.reps[0] + 1;
            let length = (*pending).min((limit - window.total) as usize);
            window.copy(distance, length);
            *pending -= length;
            continue;
        }
        if range.needs_input() {
            return Ok(None);
        }
        let position_state = window.total as usize & position_mask;
        if range.branching_bit(&mut state.is_match[state.state][position_state]) == 0 {
            state.decode_literal(range, window);
            continue;
        }
        let length;
        if range.branching_bit(&mut state.is_rep[state.state]) == 1 {
            if window.total == 0 {
                return Err(data_error("LZMA stream repeats a match before any output"));
            }
            if range.branching_bit(&mut state.is_rep0[state.state]) == 0 {
                if range.branching_bit(&mut state.is_rep0_long[state.state][position_state]) == 0 {
                    state.state = if state.state < 7 { 9 } else { 11 };
                    let byte = window.back(state.reps[0] + 1);
                    window.put(byte);
                    continue;
                }
            } else {
                let distance;
                if range.branching_bit(&mut state.is_rep1[state.state]) == 0 {
                    distance = state.reps[1];
                } else {
                    if range.branching_bit(&mut state.is_rep2[state.state]) == 0 {
                        distance = state.reps[2];
                    } else {
                        distance = state.reps[3];
                        state.reps[3] = state.reps[2];
                    }
                    state.reps[2] = state.reps[1];
                }
                state.reps[1] = state.reps[0];
                state.reps[0] = distance;
            }
            length = state.rep_length.decode(range, position_state);
            state.state = if state.state < 7 { 8 } else { 11 };
        } else {
            state.reps[3] = state.reps[2];
            state.reps[2] = state.reps[1];
            state.reps[1] = state.reps[0];
            length = state.length.decode(range, position_state);
            state.state = if state.state < 7 { 7 } else { 10 };
            let distance = state.decode_distance(range, length);
            if distance == 0xFFFF_FFFF {
                range.ran_past_input()?;
                if !range.finished_cleanly() {
                    return Err(data_error(
                        "LZMA end marker leaves the range coder unfinished",
                    ));
                }
                return Ok(Some(Stop::EndMarker));
            }
            if distance >= state.dictionary {
                return Err(data_error("LZMA match reaches past its dictionary"));
            }
            state.reps[0] = distance;
        }
        let distance = state.reps[0] + 1;
        if !window.reaches(distance) {
            return Err(data_error(
                "LZMA match reaches before the start of the output",
            ));
        }
        *pending = length + MATCH_LENGTH_MIN;
    }
    range.ran_past_input()?;
    Ok(Some(Stop::Limit))
}

pub(crate) struct RawLzmaReader<R> {
    state: ChunkState,
    range: RangeDecoder<R>,
    window: Window,
    decoded_size: u64,
    pending: usize,
    started: bool,
    finished: bool,
    output_start: u64,
}

impl<R: Read> RawLzmaReader<R> {
    pub(crate) fn new(
        input: R,
        properties: Properties,
        dictionary: u32,
        decoded_size: u64,
    ) -> Self {
        let window = u64::from(dictionary)
            .max(DICTIONARY_MIN as u64)
            .min(decoded_size.max(1)) as usize;
        RawLzmaReader {
            state: ChunkState::new(properties, dictionary as usize),
            range: RangeDecoder::new(input),
            window: Window::new(window),
            decoded_size,
            pending: 0,
            started: false,
            finished: false,
            output_start: 0,
        }
    }
}

impl<R: Read> Read for RawLzmaReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.finished || buffer.is_empty() {
            return Ok(0);
        }
        if !self.started {
            self.range.start()?;
            self.started = true;
        }
        if self.output_start == self.window.total {
            let step = OUTPUT_CHUNK
                .min(buffer.len())
                .min(self.window.size - COPY_CHUNK) as u64;
            let limit = self
                .window
                .total
                .saturating_add(step)
                .min(self.decoded_size);
            let stop = decode(
                &mut self.state,
                &mut self.range,
                &mut self.window,
                limit,
                &mut self.pending,
            )?;
            if stop == Stop::EndMarker && self.window.total != self.decoded_size {
                return Err(data_error("LZMA end marker comes before the declared size"));
            }
            if self.window.total == self.decoded_size && self.pending > 0 {
                return Err(data_error("LZMA match runs past the declared size"));
            }
        }
        let available = (self.window.total - self.output_start) as usize;
        let count = available.min(buffer.len());
        let length = self.window.size;
        let first = (self.window.position + length - available) % length;
        let before_wrap = count.min(length - first);
        buffer[..before_wrap].copy_from_slice(&self.window.buffer[first..first + before_wrap]);
        buffer[before_wrap..count].copy_from_slice(&self.window.buffer[..count - before_wrap]);
        self.output_start += count as u64;
        self.finished = self.output_start == self.decoded_size;
        Ok(count)
    }
}

const LIBLZMA_DICTIONARY_ALIGN: usize = 16;
const CONTROL_END: u8 = 0x00;
const STORED_RESET_DICTIONARY: u8 = 0x01;
const STORED: u8 = 0x02;
const LZMA: u8 = 0x80;
const LZMA_RESET_STATE: u8 = 0xA0;
const LZMA_NEW_PROPERTIES: u8 = 0xC0;
const LZMA_RESET_DICTIONARY: u8 = 0xE0;
const LZMA2_LC_PLUS_LP_MAX: u32 = 4;

#[derive(Clone, Copy)]
enum Lzma2Chunk {
    Control,
    Stored { left: usize },
    Lzma { left: u64, input_end: u64 },
    End,
    Failed,
}

pub(crate) struct Lzma2Reader<R> {
    state: ChunkState,
    range: RangeDecoder<R>,
    window: Window,
    chunk: Lzma2Chunk,
    needs_dictionary_reset: bool,
    needs_properties: bool,
    pending: usize,
    output_start: u64,
}

impl<R: Read> Lzma2Reader<R> {
    pub(crate) fn new(input: R, dictionary: u32) -> Self {
        // EXTERNAL(liblzma lz_decoder.c): the dictionary is at least 4 KiB, rounded up to 16 bytes, and matches may reach all of it.
        let dictionary = (dictionary as usize)
            .max(DICTIONARY_MIN)
            .next_multiple_of(LIBLZMA_DICTIONARY_ALIGN);
        let properties = Properties {
            lc: 0,
            lp: 0,
            pb: 0,
        };
        Lzma2Reader {
            state: ChunkState::new(properties, dictionary),
            range: RangeDecoder::new(input),
            window: Window::new(dictionary),
            chunk: Lzma2Chunk::Control,
            needs_dictionary_reset: true,
            needs_properties: true,
            pending: 0,
            output_start: 0,
        }
    }

    fn header_byte(&mut self) -> io::Result<u8> {
        self.range
            .next_byte()?
            .ok_or_else(|| data_error("truncated LZMA2 chunk header"))
    }

    fn header_field(&mut self) -> io::Result<usize> {
        let high = self.header_byte()?;
        let low = self.header_byte()?;
        Ok(usize::from(u16::from_be_bytes([high, low])))
    }

    fn next_chunk(&mut self) -> io::Result<Lzma2Chunk> {
        let control = self
            .range
            .next_byte()?
            .ok_or_else(|| data_error("LZMA2 stream ends without its end marker"))?;
        if control == CONTROL_END {
            return Ok(Lzma2Chunk::End);
        }
        if control == STORED_RESET_DICTIONARY || control >= LZMA_RESET_DICTIONARY {
            self.needs_dictionary_reset = false;
            self.needs_properties = true;
            self.window.reset();
            self.output_start = 0;
        } else if self.needs_dictionary_reset {
            return Err(data_error(
                "LZMA2 stream does not start with a dictionary reset",
            ));
        }
        match control {
            STORED_RESET_DICTIONARY | STORED => Ok(Lzma2Chunk::Stored {
                left: self.header_field()? + 1,
            }),
            LZMA..=u8::MAX => self.lzma_chunk(control),
            _ => Err(data_error("LZMA2 control byte is reserved")),
        }
    }

    fn lzma_chunk(&mut self, control: u8) -> io::Result<Lzma2Chunk> {
        let unpacked =
            (u64::from(control & !LZMA_RESET_DICTIONARY) << 16) + self.header_field()? as u64 + 1;
        let packed = self.header_field()? as u64 + 1;
        if control >= LZMA_NEW_PROPERTIES {
            let properties = Properties::from_byte(self.header_byte()?)
                .filter(|properties| properties.lc + properties.lp <= LZMA2_LC_PLUS_LP_MAX)
                .ok_or_else(|| data_error("LZMA2 properties are out of range"))?;
            self.needs_properties = false;
            self.state.reset(properties);
        } else if self.needs_properties {
            return Err(data_error("LZMA2 chunk comes before its properties"));
        } else if control >= LZMA_RESET_STATE {
            self.state.reset(self.state.properties);
        }
        let input_end = self.range.consumed() + packed;
        self.range.input_limit = input_end;
        self.range.start()?;
        Ok(Lzma2Chunk::Lzma {
            left: unpacked,
            input_end,
        })
    }

    fn decode_lzma(&mut self, left: u64, input_end: u64, step: u64) -> io::Result<Lzma2Chunk> {
        let start = self.window.total;
        let limit = start + left.min(step);
        let stop = decode(
            &mut self.state,
            &mut self.range,
            &mut self.window,
            limit,
            &mut self.pending,
        )?;
        if stop == Stop::EndMarker {
            return Err(data_error("LZMA2 chunk holds an end marker"));
        }
        let left = left - (self.window.total - start);
        if left > 0 {
            return Ok(Lzma2Chunk::Lzma { left, input_end });
        }
        if self.pending > 0 {
            return Err(data_error("LZMA2 match runs past its chunk"));
        }
        if self.range.code != 0 {
            return Err(data_error("LZMA2 chunk leaves the range coder unfinished"));
        }
        if self.range.consumed() != input_end {
            return Err(data_error("LZMA2 chunk size disagrees with its data"));
        }
        self.range.input_limit = u64::MAX;
        Ok(Lzma2Chunk::Control)
    }

    fn advance(&mut self, step: usize) -> io::Result<Lzma2Chunk> {
        match self.chunk {
            Lzma2Chunk::Control => self.next_chunk(),
            Lzma2Chunk::Stored { left } => {
                let copied = self.range.copy_into(&mut self.window, left.min(step))?;
                Ok(match left - copied {
                    0 => Lzma2Chunk::Control,
                    left => Lzma2Chunk::Stored { left },
                })
            }
            Lzma2Chunk::Lzma { left, input_end } => self.decode_lzma(left, input_end, step as u64),
            Lzma2Chunk::End => Ok(Lzma2Chunk::End),
            Lzma2Chunk::Failed => Err(data_error("LZMA2 stream failed earlier")),
        }
    }
}

impl<R: Read> Read for Lzma2Reader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        while self.output_start == self.window.total {
            if matches!(self.chunk, Lzma2Chunk::End) {
                return Ok(0);
            }
            let step = OUTPUT_CHUNK
                .min(buffer.len())
                .min(self.window.size - COPY_CHUNK);
            match self.advance(step) {
                Ok(chunk) => self.chunk = chunk,
                Err(error) => {
                    self.chunk = Lzma2Chunk::Failed;
                    return Err(error);
                }
            }
        }
        let available = (self.window.total - self.output_start) as usize;
        let count = available.min(buffer.len());
        let length = self.window.size;
        let first = (self.window.position + length - available) % length;
        let before_wrap = count.min(length - first);
        buffer[..before_wrap].copy_from_slice(&self.window.buffer[first..first + before_wrap]);
        buffer[before_wrap..count].copy_from_slice(&self.window.buffer[..count - before_wrap]);
        self.output_start += count as u64;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(data: &[u8], lc: u32, lp: u32, pb: u32, dictionary: u32, preset: u32) -> Vec<u8> {
        let mut options = liblzma::stream::LzmaOptions::new_preset(preset).unwrap();
        options
            .literal_context_bits(lc)
            .literal_position_bits(lp)
            .position_bits(pb)
            .dict_size(dictionary);
        let mut filters = liblzma::stream::Filters::new();
        filters.lzma1(&options);
        let mut out = Vec::new();
        liblzma::read::XzEncoder::new_stream(
            data,
            liblzma::stream::Stream::new_raw_encoder(&filters).unwrap(),
        )
        .read_to_end(&mut out)
        .unwrap();
        out
    }

    fn decode_all(
        stream: &[u8],
        properties: Properties,
        dictionary: u32,
        size: u64,
        chunk: usize,
    ) -> io::Result<Vec<u8>> {
        let mut reader = RawLzmaReader::new(stream, properties, dictionary, size);
        let mut out = Vec::new();
        let mut buffer = vec![0u8; chunk];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                return Ok(out);
            }
            out.extend_from_slice(&buffer[..read]);
        }
    }

    fn samples() -> Vec<Vec<u8>> {
        let mut state = 3u32;
        let mut next = move || {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 16) as u8
        };
        vec![
            Vec::new(),
            b"a".to_vec(),
            vec![0; 100_000],
            b"abcabcabd".repeat(5_000),
            (0..70_000).map(|_| next()).collect(),
            (0..200_000)
                .map(|index| b"the quick brown fox "[index % 20] ^ (next() & 1))
                .collect(),
        ]
    }

    #[test]
    fn every_property_combination_decodes_what_liblzma_encodes() {
        let mut checked = 0;
        for data in samples() {
            for (lc, lp) in (0..=4).flat_map(|lc| (0..=4 - lc).map(move |lp| (lc, lp))) {
                for pb in 0..=4 {
                    for (dictionary, preset) in [(4096, 0), (1 << 16, 6), (1 << 20, 9)] {
                        let stream = encode(&data, lc, lp, pb, dictionary, preset);
                        let properties = Properties { lc, lp, pb };
                        for chunk in [1, 4093, 65_536] {
                            if chunk == 1 && data.len() > 10_000 {
                                continue;
                            }
                            let decoded = decode_all(
                                &stream,
                                properties,
                                dictionary,
                                data.len() as u64,
                                chunk,
                            )
                            .unwrap_or_else(|error| {
                                panic!("lc {lc} lp {lp} pb {pb} dictionary {dictionary}: {error}")
                            });
                            assert!(
                                decoded == data,
                                "lc {lc} lp {lp} pb {pb} dictionary {dictionary} chunk {chunk}"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 2000, "{checked}");
    }

    #[test]
    fn a_read_as_large_as_the_window_never_returns_bytes_a_chunked_copy_overran() {
        let data = mixed();
        for preset in 0..=9 {
            let stream = encode(&data, 3, 0, 2, 4096, preset);
            let properties = Properties {
                lc: 3,
                lp: 0,
                pb: 2,
            };
            let mut reader = RawLzmaReader::new(&stream[..], properties, 4096, data.len() as u64);
            let mut decoded = vec![0u8; 1];
            reader.read_exact(&mut decoded).unwrap();
            reader.read_to_end(&mut decoded).unwrap();
            assert!(decoded == data, "preset {preset}");
        }
    }

    #[test]
    fn chunked_match_copy_matches_a_byte_at_a_time_copy_in_every_window_state() {
        let mut checked = 0u64;
        for history in [16, 17, 31, 33] {
            let mut start = Window::new(history);
            for (index, byte) in start.buffer.iter_mut().enumerate() {
                *byte = index as u8;
            }
            for position in 0..start.size {
                for full in [false, true] {
                    let window = Window {
                        buffer: start.buffer.clone(),
                        position,
                        full,
                        ..start
                    };
                    assert_eq!(window.reaches(history), full || history <= position);
                    for distance in (1..=history).filter(|&distance| window.reaches(distance)) {
                        for length in 0..=2 * history + COPY_CHUNK {
                            let mut chunked = Window {
                                buffer: window.buffer.clone(),
                                ..window
                            };
                            let mut reference = Window {
                                buffer: window.buffer.clone(),
                                ..window
                            };
                            chunked.copy(distance, length);
                            for _ in 0..length {
                                let byte = reference.back(distance);
                                reference.put(byte);
                            }
                            assert_eq!(
                                (chunked.position, chunked.total, chunked.full),
                                (reference.position, reference.total, reference.full)
                            );
                            if length > 0 {
                                assert_eq!(chunked.last, reference.last);
                            }
                            for back in (1..=history).filter(|&back| reference.reaches(back)) {
                                assert_eq!(
                                    chunked.back(back),
                                    reference.back(back),
                                    "history {history} position {position} full {full} distance {distance} length {length} back {back}"
                                );
                            }
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 100_000, "{checked}");
    }

    #[test]
    fn an_end_marker_before_the_declared_size_is_refused() {
        let data = samples().remove(3);
        let stream = encode(&data, 3, 0, 2, 1 << 16, 6);
        let properties = Properties {
            lc: 3,
            lp: 0,
            pb: 2,
        };
        assert!(decode_all(&stream, properties, 1 << 16, data.len() as u64, 4096).unwrap() == data);
        let error =
            decode_all(&stream, properties, 1 << 16, data.len() as u64 + 1, 4096).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error.to_string().contains("end marker comes before"),
            "{error}"
        );
    }

    #[test]
    fn a_stream_cut_before_the_last_byte_it_reads_is_refused_at_every_size() {
        let data = samples().remove(5);
        let stream = encode(&data, 3, 0, 2, 1 << 16, 6);
        let properties = Properties {
            lc: 3,
            lp: 0,
            pb: 2,
        };
        let mut checked = 0;
        for size in (1..6_000).chain((6_000..data.len()).step_by(983)) {
            let mut reader = RawLzmaReader::new(&stream[..], properties, 1 << 16, size as u64);
            let mut decoded = Vec::new();
            if reader.read_to_end(&mut decoded).is_err() {
                continue;
            }
            assert!(decoded == data[..size]);
            checked += 1;
            let needed = reader.range.consumed() as usize;
            assert!(
                decode_all(&stream[..needed], properties, 1 << 16, size as u64, 65_536).unwrap()
                    == data[..size]
            );
            let error = decode_all(
                &stream[..needed - 1],
                properties,
                1 << 16,
                size as u64,
                65_536,
            )
            .expect_err(&format!(
                "size {size} decoded from {} of {needed} bytes",
                needed - 1
            ));
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
        assert!(checked > 50, "{checked}");
    }

    #[test]
    fn cut_and_damaged_streams_never_panic_or_pass_their_size() {
        let data = samples().remove(5);
        let stream = encode(&data, 3, 0, 2, 1 << 16, 6);
        let properties = Properties {
            lc: 3,
            lp: 0,
            pb: 2,
        };
        for cut in (0..stream.len())
            .step_by(97)
            .chain(stream.len() - 32..stream.len())
        {
            if let Ok(decoded) = decode_all(
                &stream[..cut],
                properties,
                1 << 16,
                data.len() as u64,
                65_536,
            ) {
                assert!(decoded == data, "cut to {cut} decoded to other bytes");
                assert!(
                    stream.len() - cut <= 8,
                    "cut to {cut} of {} decoded without its data",
                    stream.len()
                );
            }
        }
        let mut state = 11u32;
        for _ in 0..2000 {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let mut damaged = stream.clone();
            let at = 1 + (state >> 8) as usize % (damaged.len() - 1);
            damaged[at] ^= 1 << (state % 8);
            if let Ok(decoded) =
                decode_all(&damaged, properties, 1 << 16, data.len() as u64, 65_536)
            {
                assert_eq!(decoded.len(), data.len());
            }
        }
    }

    #[test]
    fn a_wide_literal_context_decodes_what_liblzma_refuses_to_write() {
        assert_eq!(
            Properties::from_byte(8),
            Some(Properties {
                lc: 8,
                lp: 0,
                pb: 0
            })
        );
        assert_eq!(Properties::from_byte(9 * 5 * 5), None);
    }

    fn lzma2_filters(
        lc: u32,
        lp: u32,
        pb: u32,
        dictionary: u32,
        preset: u32,
    ) -> liblzma::stream::Filters {
        let mut options = liblzma::stream::LzmaOptions::new_preset(preset).unwrap();
        options
            .literal_context_bits(lc)
            .literal_position_bits(lp)
            .position_bits(pb)
            .dict_size(dictionary);
        let mut filters = liblzma::stream::Filters::new();
        filters.lzma2(&options);
        filters
    }

    fn encode_lzma2(
        data: &[u8],
        lc: u32,
        lp: u32,
        pb: u32,
        dictionary: u32,
        preset: u32,
    ) -> Vec<u8> {
        let filters = lzma2_filters(lc, lp, pb, dictionary, preset);
        let mut out = Vec::new();
        liblzma::read::XzEncoder::new_stream(
            data,
            liblzma::stream::Stream::new_raw_encoder(&filters).unwrap(),
        )
        .read_to_end(&mut out)
        .unwrap();
        out
    }

    fn liblzma_lzma2(stream: &[u8], dictionary: u32) -> Option<Vec<u8>> {
        let (out, ended) = liblzma_lzma2_partial(stream, dictionary);
        ended.then_some(out)
    }

    fn liblzma_lzma2_partial(stream: &[u8], dictionary: u32) -> (Vec<u8>, bool) {
        let filters = lzma2_filters(0, 0, 0, dictionary, 0);
        let mut decoder = liblzma::stream::Stream::new_raw_decoder(&filters).unwrap();
        let mut out = Vec::new();
        loop {
            out.reserve(1 << 16);
            let before = (decoder.total_in(), decoder.total_out());
            let rest = &stream[decoder.total_in() as usize..];
            match decoder.process_vec(rest, &mut out, liblzma::stream::Action::Run) {
                Ok(liblzma::stream::Status::StreamEnd) => return (out, true),
                Ok(_) if (decoder.total_in(), decoder.total_out()) != before => {}
                _ => return (out, false),
            }
        }
    }

    fn decode_lzma2(stream: &[u8], dictionary: u32, chunk: usize) -> (Vec<u8>, io::Result<()>) {
        let mut reader = Lzma2Reader::new(stream, dictionary);
        let mut out = Vec::new();
        let mut buffer = vec![0u8; chunk];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => return (out, Ok(())),
                Ok(read) => out.extend_from_slice(&buffer[..read]),
                Err(error) => return (out, Err(error)),
            }
        }
    }

    fn lzma2_outcome(stream: &[u8], dictionary: u32, chunk: usize) -> Option<Vec<u8>> {
        let (out, result) = decode_lzma2(stream, dictionary, chunk);
        result.ok().map(|()| out)
    }

    fn assert_lzma2_agrees_with_liblzma(
        stream: &[u8],
        dictionary: u32,
        what: &str,
    ) -> Option<Vec<u8>> {
        let theirs = liblzma_lzma2(stream, dictionary);
        for chunk in [1, 7, 65_536] {
            let ours = lzma2_outcome(stream, dictionary, chunk);
            assert!(
                ours == theirs,
                "{what}, read size {chunk}: zipper {}, liblzma {}",
                ours.map_or("refuses".to_string(), |out| format!("{} bytes", out.len())),
                theirs
                    .as_ref()
                    .map_or("refuses".to_string(), |out| format!("{} bytes", out.len()))
            );
        }
        theirs
    }

    #[derive(Debug)]
    struct ChunkHead {
        control: u8,
        unpacked: usize,
        end: usize,
    }

    fn lzma2_chunks(stream: &[u8]) -> Vec<ChunkHead> {
        let mut heads = Vec::new();
        let mut at = 0;
        while let Some(&control) = stream.get(at) {
            let field = |offset: usize| -> Option<usize> {
                Some(
                    usize::from(*stream.get(at + offset)?) << 8
                        | usize::from(*stream.get(at + offset + 1)?),
                )
            };
            let (unpacked, end) = match control {
                0 => break,
                1 | 2 => {
                    let Some(size) = field(1) else { break };
                    (size + 1, at + 3 + size + 1)
                }
                0x80..=0xFF => {
                    let (Some(unpacked), Some(packed)) = (field(1), field(3)) else {
                        break;
                    };
                    let header = if control >= 0xC0 { 6 } else { 5 };
                    (
                        (usize::from(control & 0x1F) << 16) + unpacked + 1,
                        at + header + packed + 1,
                    )
                }
                _ => break,
            };
            heads.push(ChunkHead {
                control,
                unpacked,
                end,
            });
            at = end;
        }
        heads
    }

    fn lzma_chunk(control: u8, unpacked: usize, properties: Option<u8>, payload: &[u8]) -> Vec<u8> {
        let unpacked = unpacked - 1;
        let packed = payload.len() - 1;
        let mut chunk = vec![
            control | (unpacked >> 16) as u8,
            (unpacked >> 8) as u8,
            unpacked as u8,
            (packed >> 8) as u8,
            packed as u8,
        ];
        chunk.extend(properties);
        chunk.extend_from_slice(payload);
        chunk
    }

    fn stored_chunk(control: u8, data: &[u8]) -> Vec<u8> {
        let size = data.len() - 1;
        let mut chunk = vec![control, (size >> 8) as u8, size as u8];
        chunk.extend_from_slice(data);
        chunk
    }

    struct Encoded {
        properties: u8,
        payload: Vec<u8>,
        unpacked: usize,
    }

    fn one_lzma_chunk(data: &[u8], lc: u32, lp: u32, pb: u32) -> Encoded {
        let stream = encode_lzma2(data, lc, lp, pb, 1 << 16, 6);
        let heads = lzma2_chunks(&stream);
        assert_eq!(heads.len(), 1, "{heads:?}");
        assert_eq!(heads[0].control & 0xE0, 0xE0);
        assert_eq!(stream[heads[0].end..], [0]);
        Encoded {
            properties: stream[5],
            payload: stream[6..heads[0].end].to_vec(),
            unpacked: heads[0].unpacked,
        }
    }

    fn noise(seed: u32, length: usize) -> Vec<u8> {
        let mut state = seed;
        (0..length)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (state >> 16) as u8
            })
            .collect()
    }

    fn text(length: usize, seed: u32) -> Vec<u8> {
        let words: [&[u8]; 8] = [
            b"lorem ",
            b"ipsum ",
            b"dolor ",
            b"sit ",
            b"amet, ",
            b"zipper ",
            b"chunk ",
            b"dictionary\n",
        ];
        let mut out = Vec::with_capacity(length + 16);
        for byte in noise(seed, length) {
            if out.len() >= length {
                break;
            }
            out.extend_from_slice(words[usize::from(byte) % 8]);
        }
        out.truncate(length);
        out
    }

    fn mixed() -> Vec<u8> {
        [
            noise(5, 66_000),
            text(3_000, 1),
            noise(6, 140_000),
            text(3_000, 2),
            noise(7, 1_000),
        ]
        .concat()
    }

    fn assembled_stream() -> (Vec<u8>, Vec<u8>) {
        let first = text(3_001, 3);
        let stored = noise(8, 500);
        let second = text(2_000, 4);
        let reset = noise(9, 300);
        let third = text(1_500, 5);
        let a = one_lzma_chunk(&first, 0, 0, 0);
        let b = one_lzma_chunk(&second, 0, 0, 0);
        let c = one_lzma_chunk(&third, 0, 0, 0);
        let stream = [
            lzma_chunk(0xE0, a.unpacked, Some(a.properties), &a.payload),
            stored_chunk(2, &stored),
            lzma_chunk(0xA0, b.unpacked, None, &b.payload),
            stored_chunk(1, &reset),
            lzma_chunk(0xC0, c.unpacked, Some(c.properties), &c.payload),
            vec![0],
        ]
        .concat();
        (stream, [first, stored, second, reset, third].concat())
    }

    #[test]
    fn lzma2_decodes_what_liblzma_encodes_at_every_preset_and_property() {
        let mut inputs = samples();
        inputs.extend([noise(4, 200_000), mixed(), text(5 << 20, 6)]);
        let mut controls = std::collections::BTreeSet::new();
        let mut checked = 0;
        let mut check = |data: &[u8], lc, lp, pb, dictionary, preset| {
            let stream = encode_lzma2(data, lc, lp, pb, dictionary, preset);
            controls.extend(lzma2_chunks(&stream).iter().map(|head| match head.control {
                0x80..=0xFF => head.control & 0xE0,
                control => control,
            }));
            for chunk in [1, 4093, 65_536] {
                if chunk == 1 && data.len() > if preset == 0 { 300_000 } else { 10_000 } {
                    continue;
                }
                let (decoded, result) = decode_lzma2(&stream, dictionary, chunk);
                result.unwrap_or_else(|error| {
                    panic!(
                        "lc {lc} lp {lp} pb {pb} dictionary {dictionary} preset {preset}: {error}"
                    )
                });
                assert!(
                    decoded == data,
                    "lc {lc} lp {lp} pb {pb} dictionary {dictionary} preset {preset} chunk {chunk} length {} first difference {:?}",
                    data.len(),
                    decoded.iter().zip(data.iter()).position(|(a, b)| a != b)
                );
                checked += 1;
            }
        };
        for (index, data) in inputs.iter().enumerate() {
            for preset in 0..=9 {
                if data.len() > 1 << 20 && preset % 3 != 0 {
                    continue;
                }
                let dictionary = [4096, 1 << 16, 1 << 20][(index + preset as usize) % 3];
                check(data, 3, 0, 2, dictionary, preset);
            }
        }
        for data in [&inputs[5], &inputs[7]] {
            for (lc, lp) in (0..=4).flat_map(|lc| (0..=4 - lc).map(move |lp| (lc, lp))) {
                for pb in 0..=4 {
                    check(data, lc, lp, pb, 1 << 16, 6);
                }
            }
        }
        assert_eq!(
            controls.into_iter().collect::<Vec<_>>(),
            [1, 2, 0x80, 0xA0, 0xC0, 0xE0]
        );
        assert!(checked > 480, "{checked}");
    }

    #[test]
    fn lzma2_control_codes_decode_and_refuse_as_liblzma_does() {
        let (stream, data) = assembled_stream();
        assert_eq!(
            assert_lzma2_agrees_with_liblzma(&stream, 1 << 16, "assembled"),
            Some(data)
        );
        let a = one_lzma_chunk(&text(2_000, 11), 0, 0, 0);
        let e0 = lzma_chunk(0xE0, a.unpacked, Some(a.properties), &a.payload);
        let with_control = |control: u8, properties: Option<u8>| {
            lzma_chunk(control, a.unpacked, properties, &a.payload)
        };
        let stored = stored_chunk(1, b"abc");
        let cases: Vec<(&str, Vec<u8>, bool)> = vec![
            ("only the end", vec![0], true),
            ("no bytes", vec![], false),
            (
                "stored chunks",
                [stored.clone(), stored_chunk(2, b"de"), vec![0]].concat(),
                true,
            ),
            (
                "a stored chunk without a dictionary reset first",
                [stored_chunk(2, b"de"), vec![0]].concat(),
                false,
            ),
            (
                "an LZMA chunk without any reset first",
                [with_control(0x80, None), vec![0]].concat(),
                false,
            ),
            (
                "a state reset first",
                [with_control(0xA0, None), vec![0]].concat(),
                false,
            ),
            (
                "new properties without a dictionary reset first",
                [with_control(0xC0, Some(a.properties)), vec![0]].concat(),
                false,
            ),
            (
                "an LZMA chunk without properties after a stored reset",
                [stored.clone(), with_control(0x80, None), vec![0]].concat(),
                false,
            ),
            (
                "a state reset without properties after a stored reset",
                [stored.clone(), with_control(0xA0, None), vec![0]].concat(),
                false,
            ),
            (
                "new properties after a stored reset",
                [
                    stored.clone(),
                    with_control(0xC0, Some(a.properties)),
                    vec![0],
                ]
                .concat(),
                true,
            ),
            (
                "a full reset after a stored reset",
                [stored.clone(), e0.clone(), vec![0]].concat(),
                true,
            ),
            (
                "two full resets",
                [e0.clone(), e0.clone(), vec![0]].concat(),
                true,
            ),
            ("a full reset without its end", e0.clone(), false),
            ("a stored chunk without its end", stored.clone(), false),
            (
                "a reserved control 3",
                [stored.clone(), vec![3, 0, 0, 0], vec![0]].concat(),
                false,
            ),
            (
                "a reserved control 0x7F",
                [stored.clone(), vec![0x7F, 0, 0, 0], vec![0]].concat(),
                false,
            ),
            ("a reserved control first", vec![0x40, 0, 0, 0], false),
            (
                "a stored chunk shorter than its size",
                vec![1, 0, 9, b'a', b'b'],
                false,
            ),
        ];
        for (what, stream, decodes) in cases {
            let outcome = assert_lzma2_agrees_with_liblzma(&stream, 1 << 16, what);
            assert_eq!(outcome.is_some(), decodes, "{what}");
        }
    }

    #[test]
    fn a_dictionary_reset_restarts_the_position_and_previous_byte() {
        let first = [text(4_099, 12), vec![0xFF]].concat();
        let second = text(3_000, 13);
        let a = one_lzma_chunk(&first, 3, 1, 2);
        let b = one_lzma_chunk(&second, 3, 1, 2);
        let stream = [
            lzma_chunk(0xE0, a.unpacked, Some(a.properties), &a.payload),
            lzma_chunk(0xE0, b.unpacked, Some(b.properties), &b.payload),
            vec![0],
        ]
        .concat();
        assert_eq!(
            assert_lzma2_agrees_with_liblzma(&stream, 1 << 16, "two full resets"),
            Some([first, second].concat())
        );
        let stored_first = [
            stored_chunk(1, &noise(14, 4_101)),
            lzma_chunk(0xE0, b.unpacked, Some(b.properties), &b.payload),
            vec![0],
        ]
        .concat();
        assert!(
            assert_lzma2_agrees_with_liblzma(&stored_first, 1 << 16, "stored then full reset")
                .is_some()
        );
        for control in [0xA0, 0xC0] {
            let properties = (control == 0xC0).then_some(b.properties);
            let stream = [
                lzma_chunk(0xE0, a.unpacked, Some(a.properties), &a.payload),
                lzma_chunk(control, b.unpacked, properties, &b.payload),
                vec![0],
            ]
            .concat();
            assert_lzma2_agrees_with_liblzma(
                &stream,
                1 << 16,
                &format!("state reset {control:#x}"),
            );
        }
    }

    #[test]
    fn lzma2_properties_and_sizes_are_refused_as_liblzma_refuses_them() {
        let a = one_lzma_chunk(&text(2_000, 15), 0, 0, 0);
        let mut decoded = 0;
        for properties in 0..=255u8 {
            let stream = [
                lzma_chunk(0xE0, a.unpacked, Some(properties), &a.payload),
                vec![0],
            ]
            .concat();
            let outcome = assert_lzma2_agrees_with_liblzma(
                &stream,
                1 << 16,
                &format!("properties {properties}"),
            );
            let fields = Properties::from_byte(properties);
            if fields.is_none_or(|fields| fields.lc + fields.lp > 4) {
                assert!(outcome.is_none(), "properties {properties}");
            }
            decoded += usize::from(outcome.is_some());
        }
        assert!(decoded >= 1, "{decoded}");
        for (unpacked, packed) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1)] {
            let mut payload = a.payload.clone();
            if packed > 0 {
                payload.push(0);
            } else if packed < 0 {
                payload.pop();
            }
            let stream = [
                lzma_chunk(
                    0xE0,
                    (a.unpacked as isize + unpacked) as usize,
                    Some(a.properties),
                    &payload,
                ),
                vec![0],
            ]
            .concat();
            let outcome = assert_lzma2_agrees_with_liblzma(
                &stream,
                1 << 16,
                &format!("unpacked {unpacked:+} packed {packed:+}"),
            );
            assert!(outcome.is_none(), "unpacked {unpacked:+} packed {packed:+}");
        }
        let data = text(3_000, 16);
        let mut options = liblzma::stream::LzmaOptions::new_preset(6).unwrap();
        options
            .literal_context_bits(0)
            .literal_position_bits(0)
            .position_bits(0)
            .dict_size(1 << 16);
        let mut filters = liblzma::stream::Filters::new();
        filters.lzma1(&options);
        let mut with_end_marker = Vec::new();
        liblzma::read::XzEncoder::new_stream(
            &data[..],
            liblzma::stream::Stream::new_raw_encoder(&filters).unwrap(),
        )
        .read_to_end(&mut with_end_marker)
        .unwrap();
        for unpacked in [data.len(), data.len() + 1] {
            let stream = [
                lzma_chunk(0xE0, unpacked, Some(0), &with_end_marker),
                vec![0],
            ]
            .concat();
            let outcome = assert_lzma2_agrees_with_liblzma(
                &stream,
                1 << 16,
                &format!("end marker inside {unpacked}"),
            );
            assert!(outcome.is_none(), "end marker inside {unpacked}");
        }
        for zeros_after in 1..=3 {
            for padding in 0..=40 {
                let payload = [with_end_marker.clone(), vec![0; padding]].concat();
                let stream = [
                    lzma_chunk(0xE0, data.len() + zeros_after, Some(0), &payload),
                    vec![0],
                ]
                .concat();
                let what =
                    format!("{zeros_after} bytes after an end marker, {padding} bytes of padding");
                assert!(
                    assert_lzma2_agrees_with_liblzma(&stream, 1 << 16, &what).is_none(),
                    "{what}"
                );
            }
        }
    }

    struct RangeEncoder {
        low: u64,
        range: u32,
        cache: u8,
        cache_size: u64,
        out: Vec<u8>,
    }

    impl RangeEncoder {
        fn shift_low(&mut self) {
            if (self.low as u32) < 0xFF00_0000 || self.low >> 32 != 0 {
                let carry = (self.low >> 32) as u8;
                let mut byte = self.cache;
                while self.cache_size > 0 {
                    self.out.push(byte.wrapping_add(carry));
                    byte = 0xFF;
                    self.cache_size -= 1;
                }
                self.cache = (self.low >> 24) as u8;
            }
            self.cache_size += 1;
            self.low = (self.low & 0x00FF_FFFF) << 8;
        }

        fn bit(&mut self, probability: &mut u16, bit: u8) {
            let bound = (self.range >> PROBABILITY_BITS) * u32::from(*probability);
            if bit == 0 {
                self.range = bound;
                *probability += (PROBABILITY_ONE - *probability) >> MOVE_BITS;
            } else {
                self.low += u64::from(bound);
                self.range -= bound;
                *probability -= *probability >> MOVE_BITS;
            }
            while self.range < TOP {
                self.range <<= 8;
                self.shift_low();
            }
        }
    }

    fn literals_only(data: &[u8], properties: Properties) -> Vec<u8> {
        let Properties { lc, lp, pb } = properties;
        let mut encoder = RangeEncoder {
            low: 0,
            range: u32::MAX,
            cache: 0,
            cache_size: 1,
            out: Vec::new(),
        };
        let mut is_match = [PROBABILITY_ONE / 2; POSITION_STATES_MAX];
        let mut literal = vec![PROBABILITY_ONE / 2; 0x300 << (lc + lp)];
        let mut previous = 0usize;
        for (position, &byte) in data.iter().enumerate() {
            encoder.bit(&mut is_match[position & ((1 << pb) - 1)], 0);
            let context = ((position & ((1 << lp) - 1)) << lc) + (previous >> (8 - lc));
            let mut symbol = 1usize;
            for shift in (0..8).rev() {
                let bit = byte >> shift & 1;
                encoder.bit(&mut literal[0x300 * context + symbol], bit);
                symbol = symbol << 1 | usize::from(bit);
            }
            previous = usize::from(byte);
        }
        for _ in 0..5 {
            encoder.shift_low();
        }
        encoder.out
    }

    #[test]
    fn lzma2_decodes_every_literal_context_liblzma_allows_and_refuses_the_rest() {
        let data = text(300, 20);
        for byte in 0..9 * 5 * 5 {
            let properties = Properties::from_byte(byte).unwrap();
            let payload = literals_only(&data, properties);
            let stream = [lzma_chunk(0xE0, data.len(), Some(byte), &payload), vec![0]].concat();
            let outcome =
                assert_lzma2_agrees_with_liblzma(&stream, 1 << 16, &format!("properties {byte}"));
            let allowed = properties.lc + properties.lp <= 4;
            assert_eq!(outcome, allowed.then(|| data.clone()), "properties {byte}");
            let raw = decode_all(&payload, properties, 1 << 16, data.len() as u64, 4093).unwrap();
            assert!(raw == data, "raw LZMA, properties {byte}");
        }
    }

    #[test]
    fn lzma2_dictionary_limits_match_liblzma() {
        let phrase = noise(17, 6_000);
        let data = [phrase.clone(), noise(18, 3_000), phrase].concat();
        let stream = encode_lzma2(&data, 3, 0, 2, 1 << 16, 6);
        let mut outcomes = [0, 0];
        for dictionary in (0..=9_200).step_by(23).chain(8_990..9_010).chain([1 << 16]) {
            let outcome = assert_lzma2_agrees_with_liblzma(
                &stream,
                dictionary,
                &format!("dictionary {dictionary}"),
            );
            if let Some(out) = &outcome {
                assert!(*out == data);
            }
            outcomes[usize::from(outcome.is_some())] += 1;
        }
        assert!(outcomes[0] > 0 && outcomes[1] > 0, "{outcomes:?}");
    }

    #[test]
    fn every_cut_of_a_multi_chunk_lzma2_stream_is_refused() {
        let (stream, data) = assembled_stream();
        assert!(lzma2_chunks(&stream).len() == 5);
        for cut in 0..stream.len() {
            let (decoded, result) = decode_lzma2(&stream[..cut], 1 << 16, 4093);
            assert!(result.is_err(), "cut to {cut} of {} decoded", stream.len());
            assert!(
                data.starts_with(&decoded),
                "cut to {cut} returned other bytes"
            );
            assert!(liblzma_lzma2(&stream[..cut], 1 << 16).is_none());
        }
        let stream = encode_lzma2(&mixed(), 3, 0, 2, 1 << 16, 6);
        let heads = lzma2_chunks(&stream);
        let controls: Vec<u8> = heads
            .iter()
            .map(|head| head.control & if head.control >= 0x80 { 0xE0 } else { 0xFF })
            .collect();
        assert_eq!(controls, [1, 0xC0, 2, 0xA0]);
        let near_boundaries = heads
            .iter()
            .flat_map(|head| head.end.saturating_sub(40)..head.end + 8)
            .filter(|&cut| cut < stream.len());
        for cut in (0..stream.len())
            .step_by(211)
            .chain(0..40)
            .chain(near_boundaries)
        {
            assert!(
                decode_lzma2(&stream[..cut], 1 << 16, 65_536).1.is_err(),
                "cut to {cut}"
            );
        }
    }

    #[test]
    fn damaged_lzma2_streams_never_panic_or_pass_their_chunk_sizes() {
        let (assembled, _) = assembled_stream();
        let big = encode_lzma2(&[text(1 << 21, 19), mixed()].concat(), 3, 0, 2, 1 << 20, 6);
        let mut state = 23u32;
        for (stream, rounds) in [(&assembled, 3_000), (&big, 60)] {
            for _ in 0..rounds {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let mut damaged = stream.clone();
                let at = (state >> 4) as usize % damaged.len();
                damaged[at] ^= 1 + (state >> 24) as u8 % 255;
                let declared: usize = lzma2_chunks(&damaged)
                    .iter()
                    .map(|head| head.unpacked)
                    .sum();
                let (decoded, result) = decode_lzma2(&damaged, 1 << 20, 65_536);
                assert!(
                    decoded.len() <= declared,
                    "damage at {at}: {} past {declared}",
                    decoded.len()
                );
                let (partial, ended) = liblzma_lzma2_partial(&damaged, 1 << 20);
                let theirs = ended.then_some(&partial);
                assert!(
                    result.is_ok().then_some(&decoded) == theirs,
                    "damage at {at}: zipper {}, liblzma {}",
                    result.is_ok(),
                    ended
                );
                assert!(
                    partial.starts_with(&decoded),
                    "damage at {at}: zipper returned {} bytes, liblzma {}",
                    decoded.len(),
                    partial.len()
                );
            }
        }
    }

    #[test]
    fn a_chunk_declared_shorter_than_its_data_returns_nothing_decoded_past_it() {
        let data = text(60_000, 21);
        let a = one_lzma_chunk(&data, 3, 0, 2);
        for packed in [5, 6, 20, 100, a.payload.len() / 2, a.payload.len() - 1] {
            let stream = [
                lzma_chunk(0xE0, a.unpacked, Some(a.properties), &a.payload[..packed]),
                a.payload[packed..].to_vec(),
                vec![0],
            ]
            .concat();
            let (theirs, ended) = liblzma_lzma2_partial(&stream, 1 << 16);
            assert!(!ended);
            for chunk in [1, 4093, 65_536] {
                let (ours, result) = decode_lzma2(&stream, 1 << 16, chunk);
                assert!(result.is_err());
                assert!(
                    theirs.starts_with(&ours),
                    "packed {packed} read size {chunk}: zipper returned {} bytes, liblzma {}",
                    ours.len(),
                    theirs.len()
                );
            }
        }
    }

    #[test]
    fn an_lzma2_reader_keeps_refusing_after_an_error() {
        let mut reader = Lzma2Reader::new(&[2u8, 0, 0, b'a', 0][..], 1 << 16);
        let mut buffer = [0u8; 16];
        assert!(reader.read(&mut buffer).is_err());
        assert!(reader.read(&mut buffer).is_err());
    }
}

#[cfg(test)]
mod speed {
    use super::*;

    fn silesia_stream() -> (Vec<u8>, Vec<u8>, liblzma::stream::Filters) {
        let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/bench-corpora/silesia/silesia");
        let mut data = Vec::new();
        for entry in std::fs::read_dir(folder).unwrap() {
            data.extend(std::fs::read(entry.unwrap().path()).unwrap());
            if data.len() > 64 << 20 {
                break;
            }
        }
        data.truncate(64 << 20);
        let mut options = liblzma::stream::LzmaOptions::new_preset(6).unwrap();
        options.dict_size(1 << 23);
        let mut filters = liblzma::stream::Filters::new();
        filters.lzma1(&options);
        let mut stream = Vec::new();
        liblzma::read::XzEncoder::new_stream(
            &data[..],
            liblzma::stream::Stream::new_raw_encoder(&filters).unwrap(),
        )
        .read_to_end(&mut stream)
        .unwrap();
        (data, stream, filters)
    }

    #[test]
    #[ignore = "a probe, run by hand with --ignored"]
    fn decode_speed_against_liblzma() {
        let (data, stream, filters) = silesia_stream();
        for round in 0..3 {
            let started = std::time::Instant::now();
            let mut ours = Vec::with_capacity(data.len());
            RawLzmaReader::new(
                &stream[..],
                Properties {
                    lc: 3,
                    lp: 0,
                    pb: 2,
                },
                1 << 23,
                data.len() as u64,
            )
            .read_to_end(&mut ours)
            .unwrap();
            let zipper = started.elapsed();
            let started = std::time::Instant::now();
            let mut theirs = Vec::with_capacity(data.len());
            liblzma::read::XzDecoder::new_stream(
                &stream[..],
                liblzma::stream::Stream::new_raw_decoder(&filters).unwrap(),
            )
            .read_to_end(&mut theirs)
            .unwrap();
            let liblzma = started.elapsed();
            assert!(ours == data && theirs[..data.len()] == data[..]);
            eprintln!("PROBE round {round}: zipper {zipper:?}, liblzma {liblzma:?}");
        }
    }

    #[test]
    #[ignore = "a profiling loop, run by hand with --ignored"]
    fn decode_loop_for_profiling() {
        let (data, stream, _) = silesia_stream();
        eprintln!("PROFILE ready");
        for _ in 0..20 {
            let mut ours = Vec::with_capacity(data.len());
            RawLzmaReader::new(
                &stream[..],
                Properties {
                    lc: 3,
                    lp: 0,
                    pb: 2,
                },
                1 << 23,
                data.len() as u64,
            )
            .read_to_end(&mut ours)
            .unwrap();
        }
    }
}
