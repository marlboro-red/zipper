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
            let read = match self.input.read(&mut self.buffer[self.end..INPUT_CHUNK]) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                read => read?,
            };
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

impl<R> Lzma2Reader<R> {
    fn into_input(self) -> (R, Vec<u8>) {
        let RangeDecoder {
            input,
            mut buffer,
            start,
            end,
            ..
        } = self.range;
        buffer.truncate(end);
        buffer.drain(..start);
        (input, buffer)
    }
}

const XZ_MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0x00];
const XZ_FOOTER_MAGIC: [u8; 2] = *b"YZ";
const XZ_STREAM_EDGE: usize = 12;
const XZ_INDEX_INDICATOR: u8 = 0x00;
const XZ_RESERVED_CHECK_BITS: u8 = 0xF0;
const XZ_BLOCK_RESERVED_FLAGS: u8 = 0x3C;
const XZ_BLOCK_HAS_COMPRESSED_SIZE: u8 = 0x40;
const XZ_BLOCK_HAS_UNCOMPRESSED_SIZE: u8 = 0x80;
const XZ_BLOCK_FILTER_COUNT: u8 = 0x03;
const XZ_FILTER_LZMA2: u64 = 0x21;
const XZ_FILTER_RESERVED_START: u64 = 1 << 62;
const XZ_LZMA2_DICTIONARY_LARGEST: u8 = 40;
const XZ_VLI_BYTES_MAX: usize = 9;
const XZ_VLI_MAX: u64 = u64::MAX >> 1;
const XZ_UNPADDED_SIZE_MIN: u64 = 5;
const XZ_UNPADDED_SIZE_MAX: u64 = XZ_VLI_MAX & !3;
const XZ_ALIGN: u64 = 4;
const XZ_CHECK_SIZE_MAX: usize = 32;
const XZ_INPUT_CHUNK: usize = 4096;
const CRC64_POLYNOMIAL: u64 = 0xC96C_5795_D787_0F42;
const CRC64_TABLE: [u64; 256] = crc64_table();

const fn crc64_table() -> [u64; 256] {
    let mut table = [0; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = index as u64;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ CRC64_POLYNOMIAL
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
}

fn crc64(crc: u64, bytes: &[u8]) -> u64 {
    !bytes.iter().fold(!crc, |crc, &byte| {
        CRC64_TABLE[usize::from(crc as u8 ^ byte)] ^ (crc >> 8)
    })
}

fn padding_to_align(length: u64) -> u64 {
    length.wrapping_neg() % XZ_ALIGN
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum XzCheck {
    None,
    Crc32,
    Crc64,
    Sha256,
}

impl XzCheck {
    fn size(self) -> usize {
        match self {
            XzCheck::None => 0,
            XzCheck::Crc32 => 4,
            XzCheck::Crc64 => 8,
            XzCheck::Sha256 => 32,
        }
    }
}

enum XzCheckState {
    None,
    Crc32(crc32fast::Hasher),
    Crc64(u64),
    Sha256(sha2::Sha256),
}

impl XzCheckState {
    fn new(check: XzCheck) -> Self {
        match check {
            XzCheck::None => XzCheckState::None,
            XzCheck::Crc32 => XzCheckState::Crc32(crc32fast::Hasher::new()),
            XzCheck::Crc64 => XzCheckState::Crc64(0),
            XzCheck::Sha256 => XzCheckState::Sha256(sha2::Digest::new()),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            XzCheckState::None => {}
            XzCheckState::Crc32(hasher) => hasher.update(bytes),
            XzCheckState::Crc64(crc) => *crc = crc64(*crc, bytes),
            XzCheckState::Sha256(hasher) => sha2::Digest::update(hasher, bytes),
        }
    }

    fn matches(self, stored: &[u8]) -> bool {
        match self {
            XzCheckState::None => stored.is_empty(),
            XzCheckState::Crc32(hasher) => stored == hasher.finalize().to_le_bytes(),
            XzCheckState::Crc64(crc) => stored == crc.to_le_bytes(),
            XzCheckState::Sha256(hasher) => stored == sha2::Digest::finalize(hasher).as_slice(),
        }
    }
}

// SPEC(xz-file-format.txt 1.2 Multibyte Integers): seven bits a byte, low first, at most nine
// bytes, and no zero byte after the first.
fn xz_integer(mut next: impl FnMut() -> io::Result<u8>) -> io::Result<u64> {
    let mut value = 0;
    for index in 0..XZ_VLI_BYTES_MAX {
        let byte = next()?;
        value |= u64::from(byte & 0x7F) << (7 * index);
        if byte & 0x80 == 0 {
            if byte == 0 && index > 0 {
                return Err(data_error("xz integer is not in its shortest form"));
            }
            return Ok(value);
        }
    }
    Err(data_error("xz integer is longer than nine bytes"))
}

fn xz_header_integer(fields: &[u8], at: &mut usize) -> io::Result<u64> {
    xz_integer(|| {
        let byte = fields
            .get(*at)
            .copied()
            .ok_or_else(|| data_error("xz block header field runs past the header"))?;
        *at += 1;
        Ok(byte)
    })
}

// SPEC(xz-file-format.txt 2.1.1.2 Stream Flags): the first byte and the high four bits of the
// second are reserved, and the low four bits name the check.
fn xz_stream_flags(flags: [u8; 2]) -> io::Result<XzCheck> {
    if flags[0] != 0 || flags[1] & XZ_RESERVED_CHECK_BITS != 0 {
        return Err(data_error("xz stream flags set reserved bits"));
    }
    supported_xz_check(flags[1])
}

// SPEC(xz-file-format.txt 2.1.1.2 Stream Flags): a decoder that cannot verify a check should
// warn or refuse; liblzma warns, and this reader refuses.
fn supported_xz_check(id: u8) -> io::Result<XzCheck> {
    match id {
        0x00 => Ok(XzCheck::None),
        0x01 => Ok(XzCheck::Crc32),
        0x04 => Ok(XzCheck::Crc64),
        0x0A => Ok(XzCheck::Sha256),
        _ => Err(data_error("xz check type is not supported")),
    }
}

fn xz_stream_header(header: &[u8; XZ_STREAM_EDGE]) -> io::Result<([u8; 2], XzCheck)> {
    if header[..6] != XZ_MAGIC {
        return Err(data_error("xz stream header has no xz magic"));
    }
    let flags = [header[6], header[7]];
    if crc32fast::hash(&flags).to_le_bytes() != header[8..] {
        return Err(data_error("xz stream header CRC-32 does not match"));
    }
    Ok((flags, xz_stream_flags(flags)?))
}

fn xz_stream_footer(footer: &[u8; XZ_STREAM_EDGE]) -> io::Result<(u64, [u8; 2])> {
    if footer[10..] != XZ_FOOTER_MAGIC {
        return Err(data_error("xz stream footer has no footer magic"));
    }
    if crc32fast::hash(&footer[4..10]).to_le_bytes() != footer[..4] {
        return Err(data_error("xz stream footer CRC-32 does not match"));
    }
    let flags = [footer[8], footer[9]];
    // SPEC(xz-file-format.txt 2.1.2.2 Backward Size): the stored value is the index size in
    // four-byte units, less one.
    let stored = u32::from_le_bytes([footer[4], footer[5], footer[6], footer[7]]);
    Ok(((u64::from(stored) + 1) * XZ_ALIGN, flags))
}

#[derive(Debug, PartialEq, Eq)]
struct XzBlockHeader {
    compressed: Option<u64>,
    uncompressed: Option<u64>,
    dictionary: u32,
}

impl XzBlockHeader {
    fn window_within_uncompressed_size(&self) -> u32 {
        self.uncompressed.map_or(self.dictionary, |size| {
            size.min(u64::from(self.dictionary)) as u32
        })
    }
}

fn unsupported_xz_filter(id: u64) -> io::Error {
    data_error(match id {
        0x03 => "xz filter Delta is not supported yet",
        0x04 => "xz filter x86 BCJ is not supported yet",
        0x05 => "xz filter PowerPC BCJ is not supported yet",
        0x06 => "xz filter IA-64 BCJ is not supported yet",
        0x07 => "xz filter ARM BCJ is not supported yet",
        0x08 => "xz filter ARM-Thumb BCJ is not supported yet",
        0x09 => "xz filter SPARC BCJ is not supported yet",
        0x0A => "xz filter ARM64 BCJ is not supported yet",
        0x0B => "xz filter RISC-V BCJ is not supported yet",
        XZ_FILTER_LZMA2 => "xz filter chain has LZMA2 before its last filter",
        _ => "xz filter is unknown",
    })
}

// SPEC(xz-file-format.txt 5.3.1 LZMA2): one property byte; 40 means 4 GiB less one, and below
// it the dictionary is 2 or 3 times a power of two from 4 KiB.
fn xz_lzma2_dictionary(properties: &[u8]) -> io::Result<u32> {
    match *properties {
        [byte] if byte < XZ_LZMA2_DICTIONARY_LARGEST => {
            Ok((2 | u32::from(byte & 1)) << (byte / 2 + 11))
        }
        [XZ_LZMA2_DICTIONARY_LARGEST] => Ok(u32::MAX),
        _ => Err(data_error("xz LZMA2 properties are out of range")),
    }
}

fn xz_block_header(header: &[u8], check: XzCheck) -> io::Result<XzBlockHeader> {
    let (fields, stored) = header.split_at(header.len() - 4);
    if crc32fast::hash(fields).to_le_bytes() != stored {
        return Err(data_error("xz block header CRC-32 does not match"));
    }
    let flags = fields[1];
    if flags & XZ_BLOCK_RESERVED_FLAGS != 0 {
        return Err(data_error("xz block flags set reserved bits"));
    }
    let mut at = 2;
    let compressed = if flags & XZ_BLOCK_HAS_COMPRESSED_SIZE != 0 {
        let size = xz_header_integer(fields, &mut at)?;
        let room = XZ_UNPADDED_SIZE_MAX - header.len() as u64 - check.size() as u64;
        if size == 0 || size > room {
            return Err(data_error("xz block compressed size is out of range"));
        }
        Some(size)
    } else {
        None
    };
    let uncompressed = if flags & XZ_BLOCK_HAS_UNCOMPRESSED_SIZE != 0 {
        Some(xz_header_integer(fields, &mut at)?)
    } else {
        None
    };
    let filters = usize::from(flags & XZ_BLOCK_FILTER_COUNT) + 1;
    let mut chain = [(0, &fields[..0]); 4];
    for filter in &mut chain[..filters] {
        let id = xz_header_integer(fields, &mut at)?;
        if id >= XZ_FILTER_RESERVED_START {
            return Err(data_error("xz filter ID is reserved"));
        }
        let size = xz_header_integer(fields, &mut at)?;
        if size > (fields.len() - at) as u64 {
            return Err(data_error("xz filter properties run past the header"));
        }
        *filter = (id, &fields[at..at + size as usize]);
        at += size as usize;
    }
    if fields[at..].iter().any(|&byte| byte != 0) {
        return Err(data_error("xz block header padding is not zero"));
    }
    let (last, before) = chain[..filters].split_last().unwrap_or((&chain[0], &[]));
    if let Some(&(id, _)) = before.first() {
        return Err(unsupported_xz_filter(id));
    }
    if last.0 != XZ_FILTER_LZMA2 {
        return Err(unsupported_xz_filter(last.0));
    }
    Ok(XzBlockHeader {
        compressed,
        uncompressed,
        dictionary: xz_lzma2_dictionary(last.1)?,
    })
}

struct XzInput<R> {
    inner: R,
    held: Vec<u8>,
    next: usize,
    position: u64,
    limit: u64,
}

impl<R: Read> XzInput<R> {
    fn peek(&mut self) -> io::Result<Option<u8>> {
        if self.next == self.held.len() {
            self.held.resize(XZ_INPUT_CHUNK, 0);
            self.next = 0;
            let read = loop {
                match self.inner.read(&mut self.held) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        self.held.clear();
                        return Err(error);
                    }
                    Ok(read) => break read,
                }
            };
            self.held.truncate(read);
        }
        Ok(self.held.get(self.next).copied())
    }

    fn byte(&mut self, truncated: &'static str) -> io::Result<u8> {
        let byte = self.peek()?.ok_or_else(|| data_error(truncated))?;
        self.next += 1;
        self.position += 1;
        Ok(byte)
    }

    fn exact(&mut self, out: &mut [u8], truncated: &'static str) -> io::Result<()> {
        for slot in out {
            *slot = self.byte(truncated)?;
        }
        Ok(())
    }

    fn unread(&mut self, bytes: &[u8]) {
        self.held.splice(..self.next, bytes.iter().copied());
        self.next = 0;
        self.position -= bytes.len() as u64;
    }
}

impl<R: Read> Read for XzInput<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let room = (self.limit - self.position).min(buffer.len() as u64) as usize;
        let buffer = &mut buffer[..room];
        if buffer.is_empty() {
            return Ok(0);
        }
        let read = if self.next < self.held.len() {
            let run = buffer.len().min(self.held.len() - self.next);
            buffer[..run].copy_from_slice(&self.held[self.next..self.next + run]);
            self.next += run;
            run
        } else {
            self.inner.read(buffer)?
        };
        self.position += read as u64;
        Ok(read)
    }
}

#[derive(Default)]
struct XzRecords {
    count: u64,
    blocks_size: u64,
    uncompressed: u64,
    digest: sha2::Sha256,
}

impl XzRecords {
    fn add(&mut self, unpadded: u64, uncompressed: u64) -> io::Result<()> {
        let too_large = || data_error("xz stream sizes pass the largest xz integer");
        self.count += 1;
        self.blocks_size = self
            .blocks_size
            .checked_add(unpadded.next_multiple_of(XZ_ALIGN))
            .filter(|&size| size <= XZ_VLI_MAX)
            .ok_or_else(too_large)?;
        self.uncompressed = self
            .uncompressed
            .checked_add(uncompressed)
            .filter(|&size| size <= XZ_VLI_MAX)
            .ok_or_else(too_large)?;
        sha2::Digest::update(&mut self.digest, unpadded.to_le_bytes());
        sha2::Digest::update(&mut self.digest, uncompressed.to_le_bytes());
        Ok(())
    }

    fn summary(self) -> (u64, u64, u64, [u8; 32]) {
        let digest = sha2::Digest::finalize(self.digest).into();
        (self.count, self.blocks_size, self.uncompressed, digest)
    }
}

struct XzStream {
    flags: [u8; 2],
    check: XzCheck,
    blocks: XzRecords,
}

struct XzBlock<R> {
    decoder: Lzma2Reader<XzInput<R>>,
    stream: XzStream,
    header_size: u64,
    data_start: u64,
    declared: XzBlockHeader,
    decoded: u64,
    check: XzCheckState,
}

impl<R: Read> XzBlock<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.decoder.read(buffer)?;
        self.decoded += read as u64;
        if self
            .declared
            .uncompressed
            .is_some_and(|size| self.decoded > size)
        {
            return Err(data_error("xz block decodes past its uncompressed size"));
        }
        self.check.update(&buffer[..read]);
        Ok(read)
    }

    fn finish(self) -> io::Result<(XzInput<R>, XzStream)> {
        let (mut input, unread) = self.decoder.into_input();
        input.unread(&unread);
        input.limit = u64::MAX;
        let compressed = input.position - self.data_start;
        if self
            .declared
            .compressed
            .is_some_and(|size| size != compressed)
        {
            return Err(data_error(
                "xz block compressed size disagrees with its header",
            ));
        }
        if self
            .declared
            .uncompressed
            .is_some_and(|size| size != self.decoded)
        {
            return Err(data_error(
                "xz block uncompressed size disagrees with its header",
            ));
        }
        for _ in 0..padding_to_align(compressed) {
            if input.byte("truncated xz block padding")? != 0 {
                return Err(data_error("xz block padding is not zero"));
            }
        }
        let mut stored = [0; XZ_CHECK_SIZE_MAX];
        let stored = &mut stored[..self.stream.check.size()];
        input.exact(stored, "truncated xz block check")?;
        if !self.check.matches(stored) {
            return Err(data_error("xz block check does not match its data"));
        }
        let unpadded = self.header_size + compressed + stored.len() as u64;
        if unpadded > XZ_UNPADDED_SIZE_MAX {
            return Err(data_error("xz block is larger than xz allows"));
        }
        let mut stream = self.stream;
        stream.blocks.add(unpadded, self.decoded)?;
        Ok((input, stream))
    }
}

enum XzPart<R> {
    BeforeStream { input: XzInput<R>, first: bool },
    InStream { input: XzInput<R>, stream: XzStream },
    Block(Box<XzBlock<R>>),
    Done,
    Failed,
}

pub(crate) struct XzReader<R> {
    part: XzPart<R>,
}

impl<R: Read> XzReader<R> {
    pub(crate) fn new(input: R) -> Self {
        XzReader {
            part: XzPart::BeforeStream {
                input: XzInput {
                    inner: input,
                    held: Vec::new(),
                    next: 0,
                    position: 0,
                    limit: u64::MAX,
                },
                first: true,
            },
        }
    }
}

// SPEC(xz-file-format.txt 2.2 Stream Padding): streams may follow one another, separated by
// zero bytes in groups of four.
fn start_stream<R: Read>(mut input: XzInput<R>, first: bool) -> io::Result<XzPart<R>> {
    if !first {
        let mut padding = 0u64;
        while input.peek()? == Some(0) {
            input.byte("truncated xz stream padding")?;
            padding += 1;
        }
        if !padding.is_multiple_of(XZ_ALIGN) {
            return Err(data_error(
                "xz stream padding is not a multiple of four bytes",
            ));
        }
        if input.peek()?.is_none() {
            return Ok(XzPart::Done);
        }
    }
    let mut header = [0; XZ_STREAM_EDGE];
    input.exact(&mut header, "truncated xz stream header")?;
    let (flags, check) = xz_stream_header(&header)?;
    Ok(XzPart::InStream {
        input,
        stream: XzStream {
            flags,
            check,
            blocks: XzRecords::default(),
        },
    })
}

fn start_block<R: Read>(
    mut input: XzInput<R>,
    stream: XzStream,
    size_byte: u8,
) -> io::Result<XzPart<R>> {
    let header_size = (usize::from(size_byte) + 1) * XZ_ALIGN as usize;
    let mut header = vec![size_byte; header_size];
    input.exact(&mut header[1..], "truncated xz block header")?;
    let declared = xz_block_header(&header, stream.check)?;
    let data_start = input.position;
    if let Some(size) = declared.compressed {
        input.limit = data_start + size;
    }
    Ok(XzPart::Block(Box::new(XzBlock {
        decoder: Lzma2Reader::new(input, declared.window_within_uncompressed_size()),
        check: XzCheckState::new(stream.check),
        stream,
        header_size: header_size as u64,
        data_start,
        declared,
        decoded: 0,
    })))
}

struct XzIndex<'a, R> {
    input: &'a mut XzInput<R>,
    crc: crc32fast::Hasher,
    size: u64,
}

impl<R: Read> XzIndex<'_, R> {
    fn byte(&mut self) -> io::Result<u8> {
        let byte = self.input.byte("truncated xz index")?;
        self.crc.update(&[byte]);
        self.size += 1;
        Ok(byte)
    }

    fn integer(&mut self) -> io::Result<u64> {
        xz_integer(|| self.byte())
    }
}

fn read_xz_index<R: Read>(input: &mut XzInput<R>) -> io::Result<(XzRecords, u64)> {
    let mut index = XzIndex {
        input,
        crc: crc32fast::Hasher::new(),
        size: 1,
    };
    index.crc.update(&[XZ_INDEX_INDICATOR]);
    let count = index.integer()?;
    let mut records = XzRecords::default();
    for _ in 0..count {
        let unpadded = index.integer()?;
        let uncompressed = index.integer()?;
        if !(XZ_UNPADDED_SIZE_MIN..=XZ_UNPADDED_SIZE_MAX).contains(&unpadded) {
            return Err(data_error("xz index record has an impossible block size"));
        }
        records.add(unpadded, uncompressed)?;
    }
    while !index.size.is_multiple_of(XZ_ALIGN) {
        if index.byte()? != 0 {
            return Err(data_error("xz index padding is not zero"));
        }
    }
    let XzIndex { input, crc, size } = index;
    let mut stored = [0; 4];
    input.exact(&mut stored, "truncated xz index")?;
    if u32::from_le_bytes(stored) != crc.finalize() {
        return Err(data_error("xz index CRC-32 does not match"));
    }
    Ok((records, size + stored.len() as u64))
}

fn read_xz_stream_footer<R: Read>(
    input: &mut XzInput<R>,
    index_size: u64,
    header_flags: [u8; 2],
) -> io::Result<()> {
    let mut footer = [0; XZ_STREAM_EDGE];
    input.exact(&mut footer, "truncated xz stream footer")?;
    let (backward_size, flags) = xz_stream_footer(&footer)?;
    if backward_size != index_size {
        return Err(data_error("xz stream footer gives the wrong index size"));
    }
    if flags != header_flags {
        return Err(data_error("xz stream footer flags differ from its header"));
    }
    Ok(())
}

fn finish_stream<R: Read>(mut input: XzInput<R>, stream: XzStream) -> io::Result<XzPart<R>> {
    let (records, index_size) = read_xz_index(&mut input)?;
    if records.summary() != stream.blocks.summary() {
        return Err(data_error("xz index disagrees with the blocks"));
    }
    read_xz_stream_footer(&mut input, index_size, stream.flags)?;
    Ok(XzPart::BeforeStream {
        input,
        first: false,
    })
}

fn next_part<R: Read>(part: XzPart<R>) -> io::Result<XzPart<R>> {
    match part {
        XzPart::BeforeStream { input, first } => start_stream(input, first),
        XzPart::InStream { mut input, stream } => {
            match input.byte("xz stream ends before its index")? {
                XZ_INDEX_INDICATOR => finish_stream(input, stream),
                size_byte => start_block(input, stream, size_byte),
            }
        }
        XzPart::Block(block) => {
            let (input, stream) = (*block).finish()?;
            Ok(XzPart::InStream { input, stream })
        }
        XzPart::Done => Ok(XzPart::Done),
        XzPart::Failed => Err(data_error("xz stream failed earlier")),
    }
}

impl<R: Read> Read for XzReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            match &mut self.part {
                XzPart::Block(block) => match block.read(buffer) {
                    Ok(0) => {}
                    Ok(read) => return Ok(read),
                    Err(error) => {
                        self.part = XzPart::Failed;
                        return Err(error);
                    }
                },
                XzPart::Done => return Ok(0),
                _ => {}
            }
            let part = std::mem::replace(&mut self.part, XzPart::Failed);
            self.part = next_part(part)?;
        }
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

    mod xz {
        use super::*;
        use liblzma::stream::{
            Action, CONCATENATED, Check, Status, Stream, TELL_UNSUPPORTED_CHECK,
        };

        const CHECKS: [(Check, u8); 4] = [
            (Check::None, 0x00),
            (Check::Crc32, 0x01),
            (Check::Crc64, 0x04),
            (Check::Sha256, 0x0A),
        ];
        const CHECK_SIZES: [usize; 16] = [0, 4, 4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 32, 64, 64, 64];
        const SINGLE_BLOCK: usize = usize::MAX;
        const DICTIONARY_1_MIB: u8 = 16;
        const READS: [(usize, usize); 5] = [
            (usize::MAX, 1),
            (1, 7),
            (3, 4_093),
            (usize::MAX, 65_536),
            (5_000, 65_536),
        ];

        fn flush(encoder: &mut Stream, out: &mut Vec<u8>, action: Action) {
            loop {
                out.reserve(1 << 16);
                if encoder.process_vec(&[], out, action).unwrap() == Status::StreamEnd {
                    return;
                }
            }
        }

        fn encode_xz(data: &[u8], preset: u32, check: Check, block: usize) -> Vec<u8> {
            let mut encoder = Stream::new_easy_encoder(preset, check).unwrap();
            let mut out = Vec::new();
            for piece in data.chunks(block) {
                let mut rest = piece;
                while !rest.is_empty() {
                    out.reserve(1 << 16);
                    let before = encoder.total_in();
                    encoder.process_vec(rest, &mut out, Action::Run).unwrap();
                    rest = &rest[(encoder.total_in() - before) as usize..];
                }
                flush(&mut encoder, &mut out, Action::FullFlush);
            }
            flush(&mut encoder, &mut out, Action::Finish);
            out
        }

        fn liblzma_xz(stream: &[u8]) -> Option<Vec<u8>> {
            let flags = CONCATENATED | TELL_UNSUPPORTED_CHECK;
            let mut decoder = Stream::new_stream_decoder(u64::MAX, flags).unwrap();
            let mut out = Vec::new();
            loop {
                out.reserve(1 << 16);
                let before = (decoder.total_in(), decoder.total_out());
                let rest = &stream[decoder.total_in() as usize..];
                match decoder.process_vec(rest, &mut out, Action::Finish) {
                    Ok(Status::StreamEnd) => return Some(out),
                    Ok(_) if (decoder.total_in(), decoder.total_out()) != before => {}
                    _ => return None,
                }
            }
        }

        struct Trickle<'a> {
            data: &'a [u8],
            piece: usize,
        }

        impl Read for Trickle<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let count = buffer.len().min(self.piece).min(self.data.len());
                buffer[..count].copy_from_slice(&self.data[..count]);
                self.data = &self.data[count..];
                Ok(count)
            }
        }

        fn decode_xz(stream: &[u8], piece: usize, chunk: usize) -> (Vec<u8>, io::Result<()>) {
            let mut reader = XzReader::new(Trickle {
                data: stream,
                piece,
            });
            let mut out = Vec::new();
            let mut buffer = vec![0u8; chunk];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => return (out, Ok(())),
                    Ok(read) => out.extend_from_slice(&buffer[..read]),
                    Err(error) => {
                        assert!(reader.read(&mut buffer).is_err(), "an error did not latch");
                        return (out, Err(error));
                    }
                }
            }
        }

        fn xz_error(stream: &[u8]) -> String {
            decode_xz(stream, usize::MAX, 65_536)
                .1
                .expect_err("the stream decoded")
                .to_string()
        }

        fn describe(outcome: &Option<Vec<u8>>) -> String {
            outcome
                .as_ref()
                .map_or("refuses".to_string(), |out| format!("{} bytes", out.len()))
        }

        fn assert_xz_agrees_with_liblzma_in(
            stream: &[u8],
            reads: &[(usize, usize)],
            what: &str,
        ) -> Option<Vec<u8>> {
            let theirs = liblzma_xz(stream);
            for &(piece, chunk) in reads {
                let (out, result) = decode_xz(stream, piece, chunk);
                let ours = result.is_ok().then_some(out);
                assert!(
                    ours == theirs,
                    "{what}, input pieces of {piece}, reads of {chunk}: zipper {}, liblzma {}",
                    describe(&ours),
                    describe(&theirs)
                );
            }
            theirs
        }

        fn assert_xz_agrees_with_liblzma(stream: &[u8], what: &str) -> Option<Vec<u8>> {
            assert_xz_agrees_with_liblzma_in(stream, &READS, what)
        }

        fn assert_both_refuse(stream: &[u8], what: &str) {
            assert!(
                assert_xz_agrees_with_liblzma(stream, what).is_none(),
                "{what}: both decoded"
            );
        }

        fn vli(mut value: u64) -> Vec<u8> {
            let mut out = Vec::new();
            while value >= 0x80 {
                out.push(value as u8 | 0x80);
                value >>= 7;
            }
            out.push(value as u8);
            out
        }

        fn crc32_after(mut body: Vec<u8>) -> Vec<u8> {
            let crc = crc32fast::hash(&body);
            body.extend_from_slice(&crc.to_le_bytes());
            body
        }

        fn stream_header(flags: [u8; 2]) -> Vec<u8> {
            let mut header = XZ_MAGIC.to_vec();
            header.extend_from_slice(&flags);
            header.extend_from_slice(&crc32fast::hash(&flags).to_le_bytes());
            header
        }

        fn stream_footer(index_size: usize, flags: [u8; 2]) -> Vec<u8> {
            footer_with_stored_size((index_size / 4 - 1) as u32, flags)
        }

        fn footer_with_stored_size(stored: u32, flags: [u8; 2]) -> Vec<u8> {
            let body = [stored.to_le_bytes().as_slice(), &flags].concat();
            let mut footer = crc32fast::hash(&body).to_le_bytes().to_vec();
            footer.extend_from_slice(&body);
            footer.extend_from_slice(&XZ_FOOTER_MAGIC);
            footer
        }

        fn block_header(flags: u8, fields: &[u8], padding: usize) -> Vec<u8> {
            let size = (2 + fields.len() + padding + 4).next_multiple_of(4);
            let mut header = vec![(size / 4 - 1) as u8, flags];
            header.extend_from_slice(fields);
            header.resize(size - 4, 0);
            crc32_after(header)
        }

        fn lzma2_filter(dictionary: u8) -> Vec<u8> {
            vec![0x21, 1, dictionary]
        }

        fn check_value(check: u8, data: &[u8]) -> Vec<u8> {
            match check {
                0x01 => crc32fast::hash(data).to_le_bytes().to_vec(),
                0x04 => crc64(0, data).to_le_bytes().to_vec(),
                0x0A => <sha2::Sha256 as sha2::Digest>::digest(data).to_vec(),
                _ => vec![0; CHECK_SIZES[usize::from(check)]],
            }
        }

        #[derive(Clone)]
        struct Block {
            data: Vec<u8>,
            lzma2: Vec<u8>,
            compressed: bool,
            uncompressed: bool,
            filters: Vec<u8>,
            filter_count: u8,
            header_padding: usize,
        }

        impl Block {
            fn new(data: Vec<u8>, preset: u32) -> Self {
                let lzma2 = encode_lzma2(&data, 3, 0, 2, 1 << 20, preset);
                Block {
                    data,
                    lzma2,
                    compressed: false,
                    uncompressed: false,
                    filters: lzma2_filter(DICTIONARY_1_MIB),
                    filter_count: 1,
                    header_padding: 0,
                }
            }

            fn sized(mut self) -> Self {
                self.compressed = true;
                self.uncompressed = true;
                self
            }

            fn header(&self) -> Vec<u8> {
                let mut flags = self.filter_count - 1;
                let mut fields = Vec::new();
                if self.compressed {
                    flags |= 0x40;
                    fields.extend(vli(self.lzma2.len() as u64));
                }
                if self.uncompressed {
                    flags |= 0x80;
                    fields.extend(vli(self.data.len() as u64));
                }
                fields.extend_from_slice(&self.filters);
                block_header(flags, &fields, self.header_padding)
            }

            fn bytes(&self, check: u8) -> Vec<u8> {
                let mut bytes = self.header();
                bytes.extend_from_slice(&self.lzma2);
                bytes.resize(
                    bytes.len() + padding_to_align(self.lzma2.len() as u64) as usize,
                    0,
                );
                bytes.extend(check_value(check, &self.data));
                bytes
            }

            fn record(&self, check: u8) -> (u64, u64) {
                let unpadded =
                    self.header().len() + self.lzma2.len() + CHECK_SIZES[usize::from(check)];
                (unpadded as u64, self.data.len() as u64)
            }
        }

        fn index_body(records: &[(u64, u64)]) -> Vec<u8> {
            let mut body = vec![XZ_INDEX_INDICATOR];
            body.extend(vli(records.len() as u64));
            for &(unpadded, uncompressed) in records {
                body.extend(vli(unpadded));
                body.extend(vli(uncompressed));
            }
            body
        }

        fn index_from(mut body: Vec<u8>) -> Vec<u8> {
            body.resize(body.len().next_multiple_of(4), 0);
            crc32_after(body)
        }

        #[derive(Clone)]
        struct Parts {
            header: Vec<u8>,
            blocks: Vec<Vec<u8>>,
            index: Vec<u8>,
            footer: Vec<u8>,
        }

        impl Parts {
            fn new(blocks: &[Block], check: u8) -> Self {
                let records: Vec<(u64, u64)> =
                    blocks.iter().map(|block| block.record(check)).collect();
                let index = index_from(index_body(&records));
                Parts {
                    header: stream_header([0, check]),
                    blocks: blocks.iter().map(|block| block.bytes(check)).collect(),
                    footer: stream_footer(index.len(), [0, check]),
                    index,
                }
            }

            fn with_index(mut self, index: Vec<u8>) -> Self {
                self.footer = stream_footer(index.len(), [self.header[6], self.header[7]]);
                self.index = index;
                self
            }

            fn bytes(&self) -> Vec<u8> {
                [
                    self.header.clone(),
                    self.blocks.concat(),
                    self.index.clone(),
                    self.footer.clone(),
                ]
                .concat()
            }
        }

        fn two_blocks() -> Vec<Block> {
            vec![
                Block::new(text(5_000, 41), 6).sized(),
                Block::new(noise(42, 3_001), 6),
            ]
        }

        fn valid_parts() -> Parts {
            Parts::new(&two_blocks(), 0x01)
        }

        fn expected_data() -> Vec<u8> {
            [text(5_000, 41), noise(42, 3_001)].concat()
        }

        fn block_count(stream: &[u8]) -> u64 {
            let footer = &stream[stream.len() - 12..];
            let stored = u32::from_le_bytes(footer[4..8].try_into().unwrap());
            let index = stream.len() - 12 - (stored as usize + 1) * 4;
            let mut at = index + 1;
            xz_header_integer(stream, &mut at).unwrap()
        }

        #[test]
        fn crc64_matches_its_published_check_value_and_every_split() {
            assert_eq!(crc64(0, b"123456789"), 0x995D_C9BB_DF19_39FA);
            assert_eq!(crc64(0, b""), 0);
            let data = noise(43, 300);
            let whole = crc64(0, &data);
            for split in 0..=data.len() {
                assert_eq!(crc64(crc64(0, &data[..split]), &data[split..]), whole);
            }
        }

        #[test]
        fn xz_integers_decode_every_short_spelling_as_the_format_defines() {
            let reference = |bytes: &[u8]| -> Option<(u64, usize)> {
                let end = bytes.iter().take(9).position(|&byte| byte < 0x80)?;
                if end > 0 && bytes[end] == 0 {
                    return None;
                }
                let value = bytes[..=end]
                    .iter()
                    .rev()
                    .fold(0u64, |value, &byte| value << 7 | u64::from(byte & 0x7F));
                Some((value, end + 1))
            };
            let mut cases: Vec<Vec<u8>> = (0..=255u8).map(|byte| vec![byte]).collect();
            for first in 0..=255u8 {
                for second in 0..=255u8 {
                    cases.push(vec![first, second]);
                }
            }
            for length in 8..=11 {
                for last in [0x00, 0x01, 0x7F, 0x80, 0xFF] {
                    let mut bytes = vec![0xFF; length - 1];
                    bytes.push(last);
                    cases.push(bytes);
                }
            }
            for bytes in cases {
                let mut at = 0;
                let ours = xz_header_integer(&bytes, &mut at)
                    .ok()
                    .map(|value| (value, at));
                assert_eq!(ours, reference(&bytes), "{bytes:02x?}");
            }
            for value in [0, 1, 127, 128, 16_383, 16_384, u64::MAX >> 1] {
                let mut at = 0;
                assert_eq!(xz_header_integer(&vli(value), &mut at).unwrap(), value);
            }
        }

        #[test]
        fn xz_decodes_what_liblzma_encodes_at_every_preset_and_check() {
            let inputs = [
                Vec::new(),
                b"a".to_vec(),
                text(70_000, 31),
                noise(32, 20_000),
                mixed(),
            ];
            for preset in 0..=9 {
                for (index, &(check, _)) in CHECKS.iter().enumerate() {
                    for (number, data) in inputs.iter().enumerate() {
                        for block in [SINGLE_BLOCK, 30_000] {
                            let stream = encode_xz(data, preset, check, block);
                            let what = format!(
                                "preset {preset}, check {index}, input {number}, block {block}"
                            );
                            let reads = if data.len() > 100_000 {
                                &READS[1..]
                            } else {
                                &READS[..]
                            };
                            let decoded = assert_xz_agrees_with_liblzma_in(&stream, reads, &what);
                            assert!(decoded.as_ref() == Some(data), "{what}: other bytes");
                        }
                    }
                }
            }
            assert!(block_count(&encode_xz(&mixed(), 0, Check::Crc32, 30_000)) > 1);
            assert_eq!(
                block_count(&encode_xz(&[], 0, Check::Crc32, SINGLE_BLOCK)),
                0
            );
        }

        #[test]
        fn xz_streams_over_two_mebibytes_decode_in_one_and_many_blocks() {
            let data = [text(1 << 21, 33), noise(34, 300_000), text(100_000, 35)].concat();
            for (preset, (check, _), block) in [
                (0, CHECKS[1], SINGLE_BLOCK),
                (6, CHECKS[2], 1 << 20),
                (9, CHECKS[3], SINGLE_BLOCK),
                (3, CHECKS[0], 300_000),
            ] {
                let stream = encode_xz(&data, preset, check, block);
                if block != SINGLE_BLOCK {
                    assert!(block_count(&stream) > 2);
                }
                let what = format!("preset {preset}, block {block}");
                let decoded = assert_xz_agrees_with_liblzma_in(&stream, &READS[3..], &what);
                assert!(decoded.as_ref() == Some(&data), "{what}: other bytes");
            }
            let stream = encode_xz(&data, 6, Check::Crc64, 1 << 20);
            assert_eq!(decode_xz(&stream, usize::MAX, 1).0, data);
            assert_eq!(decode_xz(&stream, 1, 65_536).0, data);
        }

        #[test]
        fn blocks_with_and_without_sizes_decode_at_every_check() {
            let data = [
                text(9_000, 44),
                noise(45, 70_000),
                Vec::new(),
                b"z".to_vec(),
            ];
            for &(_, check) in &CHECKS {
                for sizes in 0..4u8 {
                    let blocks: Vec<Block> = data
                        .iter()
                        .map(|data| {
                            let mut block = Block::new(data.clone(), 6);
                            block.compressed = sizes & 1 != 0;
                            block.uncompressed = sizes & 2 != 0;
                            block
                        })
                        .collect();
                    let stream = Parts::new(&blocks, check).bytes();
                    let what = format!("check {check}, sizes {sizes}");
                    let decoded = assert_xz_agrees_with_liblzma(&stream, &what);
                    assert!(decoded == Some(data.concat()), "{what}: other bytes");
                }
            }
            let mut padded = Block::new(text(2_000, 46), 6).sized();
            padded.header_padding = 37;
            let stream = Parts::new(&[padded], 0x0A).bytes();
            assert!(assert_xz_agrees_with_liblzma(&stream, "padded header").is_some());
        }

        #[test]
        fn concatenated_streams_and_stream_padding_decode_as_liblzma_does() {
            let one = valid_parts().bytes();
            let empty = Parts::new(&[], 0x04).bytes();
            let other = encode_xz(&text(4_000, 47), 6, Check::Sha256, 1_000);
            let zeros = |count: usize| vec![0u8; count];
            let accepted = [
                (one.clone(), "one stream"),
                (empty.clone(), "a stream without blocks"),
                ([one.clone(), zeros(4)].concat(), "four bytes of padding"),
                (
                    [one.clone(), zeros(1_024)].concat(),
                    "a kibibyte of padding",
                ),
                ([one.clone(), other.clone()].concat(), "two streams"),
                (
                    [
                        one.clone(),
                        zeros(8),
                        empty.clone(),
                        zeros(4),
                        other.clone(),
                    ]
                    .concat(),
                    "three streams with padding",
                ),
            ];
            for (stream, what) in accepted {
                assert!(
                    assert_xz_agrees_with_liblzma(&stream, what).is_some(),
                    "{what}"
                );
            }
            let refused = [
                (Vec::new(), "empty input"),
                (zeros(4), "only padding"),
                (
                    [zeros(4), one.clone()].concat(),
                    "padding before the first stream",
                ),
                ([one.clone(), zeros(1)].concat(), "one byte of padding"),
                ([one.clone(), zeros(3)].concat(), "three bytes of padding"),
                ([one.clone(), zeros(5)].concat(), "five bytes of padding"),
                ([one.clone(), vec![1]].concat(), "a garbage byte"),
                (
                    [one.clone(), zeros(4), vec![1]].concat(),
                    "garbage after padding",
                ),
                (
                    [one.clone(), b"YZ".to_vec()].concat(),
                    "footer magic after the stream",
                ),
                (
                    [one.clone(), zeros(2), other.clone()].concat(),
                    "short padding between",
                ),
                (
                    [one.clone(), other[..12].to_vec()].concat(),
                    "a second header alone",
                ),
                (
                    [one.clone(), one[..one.len() - 1].to_vec()].concat(),
                    "a cut second stream",
                ),
            ];
            for (stream, what) in refused {
                assert_both_refuse(&stream, what);
            }
        }

        #[test]
        fn every_check_id_decodes_or_refuses_as_liblzma_does() {
            for check in 0..16u8 {
                let stream = Parts::new(&two_blocks(), check).bytes();
                let decoded = assert_xz_agrees_with_liblzma(&stream, &format!("check {check}"));
                let supported = CHECKS.iter().any(|&(_, id)| id == check);
                assert_eq!(decoded.is_some(), supported, "check {check}");
                if !supported {
                    assert!(xz_error(&stream).contains("check type is not supported"));
                }
            }
        }

        #[test]
        fn stream_header_and_footer_damage_is_refused_as_liblzma_refuses_it() {
            let valid = valid_parts();
            assert_eq!(
                assert_xz_agrees_with_liblzma(&valid.bytes(), "valid"),
                Some(expected_data())
            );
            let edit = |change: &dyn Fn(&mut Parts)| {
                let mut parts = valid.clone();
                change(&mut parts);
                parts.bytes()
            };
            let cases: [(&str, Vec<u8>); 12] = [
                ("bad magic", edit(&|p| p.header[0] ^= 1)),
                ("bad last magic byte", edit(&|p| p.header[5] = 1)),
                ("bad header CRC", edit(&|p| p.header[8] ^= 1)),
                (
                    "reserved first flag byte",
                    edit(&|p| p.header = stream_header([1, 1])),
                ),
                (
                    "reserved check bits",
                    edit(&|p| p.header = stream_header([0, 0x11])),
                ),
                (
                    "footer flags differ",
                    edit(&|p| p.footer = stream_footer(p.index.len(), [0, 4])),
                ),
                ("bad footer magic", edit(&|p| p.footer[10] = b'Z')),
                ("bad footer CRC", edit(&|p| p.footer[0] ^= 1)),
                (
                    "footer reserved bits",
                    edit(&|p| p.footer = stream_footer(p.index.len(), [0x80, 1])),
                ),
                (
                    "backward size too large",
                    edit(&|p| p.footer = stream_footer(p.index.len() + 4, [0, 1])),
                ),
                (
                    "backward size too small",
                    edit(&|p| p.footer = stream_footer(p.index.len() - 4, [0, 1])),
                ),
                (
                    "largest backward size",
                    edit(&|p| p.footer = footer_with_stored_size(u32::MAX, [0, 1])),
                ),
            ];
            for (what, stream) in cases {
                assert_both_refuse(&stream, what);
            }
            let reserved = edit(&|p| p.header = stream_header([0, 0x11]));
            assert!(xz_error(&reserved).contains("reserved bits"));
        }

        #[test]
        fn an_index_that_disagrees_with_the_blocks_is_refused_as_liblzma_refuses_it() {
            let blocks = two_blocks();
            let records: Vec<(u64, u64)> = blocks.iter().map(|block| block.record(0x01)).collect();
            let (a, b) = (records[0], records[1]);
            let valid = Parts::new(&blocks, 0x01);
            let with_records = |records: &[(u64, u64)]| {
                valid
                    .clone()
                    .with_index(index_from(index_body(records)))
                    .bytes()
            };
            let with_body = |body: Vec<u8>| valid.clone().with_index(index_from(body)).bytes();
            assert!(assert_xz_agrees_with_liblzma(&with_records(&records), "valid").is_some());
            let mut cases = vec![
                ("one record short", with_records(&[a])),
                ("one record over", with_records(&[a, b, b])),
                ("no records", with_records(&[])),
                ("swapped records", with_records(&[b, a])),
                ("unpadded one more", with_records(&[(a.0 + 1, a.1), b])),
                ("unpadded four more", with_records(&[(a.0 + 4, a.1), b])),
                ("unpadded one less", with_records(&[a, (b.0 - 1, b.1)])),
                ("uncompressed one more", with_records(&[a, (b.0, b.1 + 1)])),
                ("uncompressed one less", with_records(&[(a.0, a.1 - 1), b])),
                (
                    "sizes moved between records",
                    with_records(&[(a.0 + 4, a.1 + 1), (b.0 - 4, b.1 - 1)]),
                ),
                ("unpadded below five", with_records(&[(4, a.1), b])),
                (
                    "count not in shortest form",
                    with_body([vec![0, 0x82, 0x00], index_body(&records)[2..].to_vec()].concat()),
                ),
                (
                    "count longer than nine bytes",
                    with_body(
                        [
                            vec![0],
                            vec![0x80; 9],
                            vec![2],
                            index_body(&records)[2..].to_vec(),
                        ]
                        .concat(),
                    ),
                ),
            ];
            let mut nonzero_padding = index_body(&records);
            nonzero_padding.resize(nonzero_padding.len().next_multiple_of(4), 0);
            assert!(
                nonzero_padding.len() > index_body(&records).len(),
                "no padding to damage"
            );
            *nonzero_padding.last_mut().unwrap() = 1;
            cases.push((
                "nonzero index padding",
                valid
                    .clone()
                    .with_index(crc32_after(nonzero_padding))
                    .bytes(),
            ));
            let mut bad_crc = valid.clone();
            *bad_crc.index.last_mut().unwrap() ^= 1;
            cases.push(("bad index CRC", bad_crc.bytes()));
            let mut bad_indicator = valid.clone();
            bad_indicator.index[0] = 1;
            cases.push(("index with a block header size", bad_indicator.bytes()));
            for (what, stream) in cases {
                assert_both_refuse(&stream, what);
            }
        }

        #[test]
        fn block_header_damage_is_refused_as_liblzma_refuses_it() {
            let base = Block::new(text(5_000, 48), 6).sized();
            let stream_of = |block: &Block| Parts::new(std::slice::from_ref(block), 0x04).bytes();
            let with_header = |header: Vec<u8>| {
                let mut parts = Parts::new(std::slice::from_ref(&base), 0x04);
                let old = base.header().len();
                parts.blocks[0] = [header, parts.blocks[0][old..].to_vec()].concat();
                parts.bytes()
            };
            let far_matches = |dictionary: u8| {
                let half = noise(60, 6_000);
                let mut block = Block::new([half.clone(), half].concat(), 6).sized();
                block.filters = lzma2_filter(dictionary);
                stream_of(&block)
            };
            let raw_header =
                |flags: u8, fields: Vec<u8>| with_header(block_header(flags, &fields, 0));
            let sizes = [vli(base.lzma2.len() as u64), vli(base.data.len() as u64)].concat();
            let lzma2 = lzma2_filter(DICTIONARY_1_MIB);
            assert!(assert_xz_agrees_with_liblzma(&stream_of(&base), "valid").is_some());
            assert!(
                assert_xz_agrees_with_liblzma(
                    &raw_header(0xC0, [sizes.clone(), lzma2.clone()].concat()),
                    "rebuilt"
                )
                .is_some()
            );
            let mut bad_crc = base.header();
            *bad_crc.last_mut().unwrap() ^= 1;
            let mut nonzero_padding = base.header();
            let crc_at = nonzero_padding.len() - 4;
            assert!(
                crc_at > 2 + sizes.len() + lzma2.len(),
                "no padding to damage"
            );
            nonzero_padding[crc_at - 1] = 1;
            let nonzero_padding = crc32_after(nonzero_padding[..crc_at].to_vec());
            let compressed = base.lzma2.len() as u64;
            let uncompressed = base.data.len() as u64;
            let cases = vec![
                ("bad header CRC", with_header(bad_crc)),
                ("nonzero header padding", with_header(nonzero_padding)),
                (
                    "reserved flag 0x04",
                    raw_header(0xC4, [sizes.clone(), lzma2.clone()].concat()),
                ),
                (
                    "reserved flag 0x20",
                    raw_header(0xE0, [sizes.clone(), lzma2.clone()].concat()),
                ),
                (
                    "compressed size one more",
                    raw_header(
                        0xC0,
                        [vli(compressed + 1), vli(uncompressed), lzma2.clone()].concat(),
                    ),
                ),
                (
                    "compressed size one less",
                    raw_header(
                        0xC0,
                        [vli(compressed - 1), vli(uncompressed), lzma2.clone()].concat(),
                    ),
                ),
                (
                    "compressed size zero",
                    raw_header(0xC0, [vli(0), vli(uncompressed), lzma2.clone()].concat()),
                ),
                (
                    "compressed size largest",
                    raw_header(0x40, [vli(u64::MAX >> 1), lzma2.clone()].concat()),
                ),
                (
                    "uncompressed size one more",
                    raw_header(
                        0xC0,
                        [vli(compressed), vli(uncompressed + 1), lzma2.clone()].concat(),
                    ),
                ),
                (
                    "uncompressed size one less",
                    raw_header(
                        0xC0,
                        [vli(compressed), vli(uncompressed - 1), lzma2.clone()].concat(),
                    ),
                ),
                (
                    "uncompressed size zero",
                    raw_header(0x80, [vli(0), lzma2.clone()].concat()),
                ),
                (
                    "size not in shortest form",
                    raw_header(0x80, [vec![0x88, 0xA7, 0x00], lzma2.clone()].concat()),
                ),
                (
                    "filter ID reserved",
                    raw_header(0x00, [vli(1 << 62), vec![1, 16]].concat()),
                ),
                ("filter unknown", raw_header(0x00, vec![0x30, 1, 16])),
                ("LZMA2 without properties", raw_header(0x00, vec![0x21, 0])),
                (
                    "LZMA2 with two property bytes",
                    raw_header(0x00, vec![0x21, 2, 16, 0]),
                ),
                (
                    "LZMA2 properties past the header",
                    raw_header(0x00, vec![0x21, 9, 16]),
                ),
                (
                    "LZMA2 twice",
                    raw_header(0x01, [lzma2.clone(), lzma2.clone()].concat()),
                ),
                ("one filter missing", raw_header(0x01, lzma2.clone())),
                (
                    "header too short for its filter",
                    with_header(crc32_after(vec![1, 0, 0x21, 1])),
                ),
                ("dictionary smaller than the matches", far_matches(0)),
            ];
            for (what, stream) in cases {
                assert_both_refuse(&stream, what);
            }
            let mut padded = base.clone();
            padded.lzma2.push(0);
            assert_both_refuse(&stream_of(&padded), "data after the LZMA2 end");
            let mut cut = base.clone();
            cut.lzma2.pop();
            cut.compressed = false;
            assert_xz_agrees_with_liblzma(&stream_of(&cut), "LZMA2 without its end");
            for dictionary in [13, 40] {
                assert!(
                    assert_xz_agrees_with_liblzma(&far_matches(dictionary), "far matches")
                        .is_some(),
                    "dictionary {dictionary}"
                );
            }
        }

        #[test]
        fn every_flag_byte_and_dictionary_byte_agrees_with_liblzma() {
            let base = Block::new(text(3_000, 49), 6);
            let mut outcomes = [0, 0];
            for flags in 0..=255u8 {
                let mut fields = Vec::new();
                if flags & 0x40 != 0 {
                    fields.extend(vli(base.lzma2.len() as u64));
                }
                if flags & 0x80 != 0 {
                    fields.extend(vli(base.data.len() as u64));
                }
                for _ in 0..=flags & 3 {
                    fields.extend(lzma2_filter(DICTIONARY_1_MIB));
                }
                let mut parts = Parts::new(std::slice::from_ref(&base), 0x01);
                let old = base.header().len();
                parts.blocks[0] = [
                    block_header(flags, &fields, 0),
                    parts.blocks[0][old..].to_vec(),
                ]
                .concat();
                let records = [(
                    (parts.blocks[0].len() - padding_to_align(base.lzma2.len() as u64) as usize)
                        as u64,
                    base.data.len() as u64,
                )];
                let stream = parts.with_index(index_from(index_body(&records))).bytes();
                let decoded = assert_xz_agrees_with_liblzma_in(
                    &stream,
                    &READS[3..],
                    &format!("flags {flags:#04x}"),
                );
                outcomes[usize::from(decoded.is_some())] += 1;
            }
            assert_eq!(outcomes, [252, 4]);
            let mut outcomes = [0, 0];
            for dictionary in 0..=255u8 {
                let mut block = base.clone().sized();
                block.filters = lzma2_filter(dictionary);
                let stream = Parts::new(&[block], 0x01).bytes();
                let decoded = assert_xz_agrees_with_liblzma_in(
                    &stream,
                    &READS[3..],
                    &format!("dictionary {dictionary}"),
                );
                outcomes[usize::from(decoded.is_some())] += 1;
            }
            assert!(outcomes[1] > 20 && outcomes[0] > 200, "{outcomes:?}");
        }

        #[test]
        fn a_block_check_that_does_not_match_its_data_is_refused() {
            for &(_, check) in &CHECKS[1..] {
                let parts = Parts::new(&two_blocks(), check);
                for (block, at) in [(0, 1), (1, CHECK_SIZES[usize::from(check)])] {
                    let mut damaged = parts.clone();
                    let end = damaged.blocks[block].len();
                    damaged.blocks[block][end - at] ^= 0x40;
                    let stream = damaged.bytes();
                    assert_both_refuse(&stream, &format!("check {check}, block {block}"));
                    assert!(xz_error(&stream).contains("check does not match"));
                }
            }
            let mut padded = Parts::new(&two_blocks(), 0x00);
            let block = &two_blocks()[1];
            assert!(
                padding_to_align(block.lzma2.len() as u64) > 0,
                "no block padding to damage"
            );
            let at = block.header().len() + block.lzma2.len();
            padded.blocks[1][at] = 1;
            assert_both_refuse(&padded.bytes(), "nonzero block padding");
        }

        #[test]
        fn a_filter_other_than_lzma2_is_refused_by_name() {
            let base = Block::new(text(2_000, 50), 6);
            for (filters, count, name) in [
                (vec![0x03, 1, 0], 2, "Delta"),
                (vec![0x04, 0], 2, "x86 BCJ"),
                (vec![0x07, 0], 2, "ARM BCJ"),
                (vec![0x0A, 0], 2, "ARM64 BCJ"),
                (vec![0x0B, 0], 2, "RISC-V BCJ"),
                (vec![0x04, 0, 0x03, 1, 0], 3, "x86 BCJ"),
            ] {
                let mut block = base.clone();
                block.filters = [filters, lzma2_filter(DICTIONARY_1_MIB)].concat();
                block.filter_count = count;
                let error = xz_error(&Parts::new(&[block], 0x01).bytes());
                assert!(
                    error.contains(name) && error.contains("not supported"),
                    "{name}: {error}"
                );
            }
            let mut alone = base.clone();
            alone.filters = vec![0x03, 1, 0];
            assert!(xz_error(&Parts::new(&[alone], 0x01).bytes()).contains("Delta"));
        }

        #[test]
        fn a_block_never_returns_more_than_its_declared_uncompressed_size() {
            let data = text(300_000, 61);
            let mut block = Block::new(data.clone(), 6).sized();
            for declared in [0, 1, 1_000, 70_000, data.len() - 1] {
                block.data = data[..declared].to_vec();
                let stream = Parts::new(&[block.clone()], 0x01).bytes();
                assert_both_refuse(&stream, &format!("declared {declared}"));
                for chunk in [1, 65_536, 1 << 20] {
                    let (decoded, result) = decode_xz(&stream, usize::MAX, chunk);
                    assert!(result.is_err());
                    assert!(
                        decoded.len() <= declared,
                        "declared {declared}, reads of {chunk}: returned {}",
                        decoded.len()
                    );
                }
            }
        }

        struct Interrupting<'a> {
            data: &'a [u8],
            piece: usize,
            calls: usize,
            every: usize,
        }

        impl Read for Interrupting<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.calls += 1;
                if self.calls.is_multiple_of(self.every) {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let count = buffer.len().min(self.piece).min(self.data.len());
                buffer[..count].copy_from_slice(&self.data[..count]);
                self.data = &self.data[count..];
                Ok(count)
            }
        }

        #[test]
        fn an_interrupted_read_is_retried_and_never_fails_the_stream() {
            let data = mixed();
            let stream = encode_xz(&data, 6, Check::Crc64, 100_000);
            let lzma2 = encode_lzma2(&data, 3, 0, 2, 1 << 20, 6);
            for every in [2, 3, 5, 50] {
                for piece in [1, 4_096, usize::MAX] {
                    let input = |data| Interrupting {
                        data,
                        piece,
                        calls: 0,
                        every,
                    };
                    let mut out = Vec::new();
                    XzReader::new(input(&stream))
                        .read_to_end(&mut out)
                        .unwrap_or_else(|error| {
                            panic!("xz, every {every}, piece {piece}: {error}")
                        });
                    assert!(out == data, "xz, every {every}, piece {piece}: other bytes");
                    out.clear();
                    Lzma2Reader::new(input(&lzma2), 1 << 20)
                        .read_to_end(&mut out)
                        .unwrap_or_else(|error| {
                            panic!("lzma2, every {every}, piece {piece}: {error}")
                        });
                    assert!(
                        out == data,
                        "lzma2, every {every}, piece {piece}: other bytes"
                    );
                }
            }
        }

        fn multi_block_stream() -> (Vec<u8>, Vec<u8>) {
            let blocks = vec![
                Block::new(text(3_000, 51), 6).sized(),
                Block::new(noise(52, 700), 0),
                Block::new(text(2_500, 53), 6),
                Block::new(Vec::new(), 6).sized(),
            ];
            let data = blocks
                .iter()
                .map(|block| block.data.clone())
                .collect::<Vec<_>>()
                .concat();
            (Parts::new(&blocks, 0x0A).bytes(), data)
        }

        #[test]
        fn every_cut_of_a_multi_block_stream_is_refused() {
            let built = multi_block_stream();
            let encoded = {
                let data = [text(6_000, 54), noise(55, 900)].concat();
                (encode_xz(&data, 6, Check::Crc32, 2_000), data)
            };
            assert!(block_count(&encoded.0) > 2);
            for (stream, data) in [built, encoded] {
                assert_eq!(liblzma_xz(&stream).as_ref(), Some(&data));
                for cut in 0..stream.len() {
                    let reads: &[(usize, usize)] = if cut % 7 == 0 { &READS } else { &READS[3..4] };
                    for &(piece, chunk) in reads {
                        let (decoded, result) = decode_xz(&stream[..cut], piece, chunk);
                        assert!(result.is_err(), "cut to {cut} of {} decoded", stream.len());
                        assert!(
                            data.starts_with(&decoded),
                            "cut to {cut} returned other bytes"
                        );
                    }
                    assert!(liblzma_xz(&stream[..cut]).is_none());
                }
            }
        }

        #[test]
        fn single_byte_damage_never_panics_and_agrees_with_liblzma() {
            let (built, _) = multi_block_stream();
            let unchecked = encode_xz(
                &[text(8_000, 56), noise(57, 2_000)].concat(),
                6,
                Check::None,
                3_000,
            );
            let checked = encode_xz(&mixed(), 1, Check::Crc64, 50_000);
            let mut state = 59u32;
            let mut outcomes = [0, 0];
            for (stream, rounds) in [(&built, 1_500), (&unchecked, 1_500), (&checked, 200)] {
                for _ in 0..rounds {
                    state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                    let mut damaged = stream.clone();
                    let at = (state >> 4) as usize % damaged.len();
                    damaged[at] ^= 1 + (state >> 24) as u8 % 255;
                    let decoded = assert_xz_agrees_with_liblzma_in(
                        &damaged,
                        &READS[2..4],
                        &format!("damage at {at}"),
                    );
                    outcomes[usize::from(decoded.is_some())] += 1;
                }
            }
            assert!(outcomes[0] > 2_000, "{outcomes:?}");
        }
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
