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

    fn bits(&self) -> Bits<'_> {
        Bits {
            input: &self.buffer,
            position: self.start,
            end: self.end,
            room: if self.ended {
                self.end + SYMBOL_INPUT_MAX
            } else {
                self.end
            },
            range: self.range,
            code: self.code,
        }
    }

    fn keep(&mut self, (position, range, code): (usize, u32, u32)) {
        self.start = position;
        self.range = range;
        self.code = code;
    }

    fn start(&mut self) -> io::Result<()> {
        self.refill()?;
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
    pub(crate) fn new(properties: Properties, dictionary: u32) -> Self {
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
            dictionary: (dictionary as usize).max(DICTIONARY_MIN),
        };
        state.reset(properties);
        state
    }

    pub(crate) fn reset(&mut self, properties: Properties) {
        let half = PROBABILITY_ONE / 2;
        self.properties = properties;
        self.literal = vec![half; 0x300 << (properties.lc + properties.lp)];
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
            None if range.ended => return Err(data_error("truncated LZMA stream")),
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
            state: ChunkState::new(properties, dictionary),
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
            let step = OUTPUT_CHUNK.min(buffer.len()).min(self.window.size) as u64;
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
