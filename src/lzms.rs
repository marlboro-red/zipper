use crate::xpress::{Huffman, LONGEST_CODE, copy_match};
use crate::{Field, field_check};

const LITERALS: usize = 256;
const LENGTH_SYMBOLS: usize = 54;
const POSITION_SYMBOLS: usize = 799;
const POWER_SYMBOLS: usize = 8;
const FEWEST_POSITION_SYMBOLS: usize = 2;
const PROBABILITY_BITS: u32 = 6;
const PROBABILITY_LIMIT: u32 = 1 << PROBABILITY_BITS;
const INITIAL_PROBABILITY: u32 = 48;
const INITIAL_HISTORY: u64 = 0x5555_5555;
const REPEATS: usize = 3;
const MAIN_STATES: usize = 16;
const MATCH_STATES: usize = 32;
const REPEAT_STATES: usize = 64;
const LITERAL_REBUILD: u32 = 1024;
const POSITION_REBUILD: u32 = 1024;
const LENGTH_REBUILD: u32 = 512;
const POWER_REBUILD: u32 = 512;
const RANGE_NORMALIZE: u32 = 0xFFFF;
const SYMBOL_BITS: u32 = 10;
const LAST_LEVEL_WITH_CHILDREN: usize = LONGEST_CODE as usize - 1;
const X86_HISTORY: usize = 1 << 16;
const X86_WINDOW: i32 = 65535;
const X86_TRANSLATION_REACH: i32 = 1023;
const X86_SHORTEST_FILTERED: usize = 17;
const X86_TAIL: usize = 16;
const X86_OPERAND: usize = 4;

// SPEC(wimlib lzms_common): runs of position slots that share a count of direct bits.
const POSITION_RUNS: [u8; 31] = [
    8, 0, 9, 7, 10, 15, 15, 20, 20, 30, 33, 40, 42, 45, 60, 73, 80, 85, 95, 105, 6, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 1,
];

// SPEC(wimlib lzms_common): direct bits of each length slot.
const LENGTH_DIRECT_BITS: [u8; LENGTH_SYMBOLS] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2,
    2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 4, 5, 5, 6, 7, 8, 9, 10, 16, 30,
];

const POSITION_DIRECT_BITS: [u8; POSITION_SYMBOLS] = {
    let mut bits = [0u8; POSITION_SYMBOLS];
    let mut slot = 0;
    let mut run = 0;
    while run < POSITION_RUNS.len() {
        let mut left = POSITION_RUNS[run];
        while left > 0 {
            bits[slot] = run as u8;
            slot += 1;
            left -= 1;
        }
        run += 1;
    }
    bits
};

const fn bases<const N: usize>(direct_bits: &[u8; N]) -> [u32; N] {
    let mut bases = [0u32; N];
    let mut sum = 1u32;
    let mut slot = 0;
    while slot < N {
        bases[slot] = sum;
        sum += 1 << direct_bits[slot];
        slot += 1;
    }
    bases
}

const POSITION_BASE: [u32; POSITION_SYMBOLS] = bases(&POSITION_DIRECT_BITS);
const LENGTH_BASE: [u32; LENGTH_SYMBOLS] = bases(&LENGTH_DIRECT_BITS);

fn position_symbols(size: usize) -> usize {
    let largest = size.saturating_sub(1) as u64;
    let used = POSITION_BASE
        .iter()
        .take_while(|&&base| u64::from(base) <= largest)
        .count();
    used.max(FEWEST_POSITION_SYMBOLS)
}

#[derive(Clone, Copy)]
struct Probability {
    zeros: u32,
    history: u64,
}

impl Probability {
    const INITIAL: Probability = Probability {
        zeros: INITIAL_PROBABILITY,
        history: INITIAL_HISTORY,
    };

    fn of_zero(self) -> u32 {
        self.zeros.clamp(1, PROBABILITY_LIMIT - 1)
    }

    fn update(&mut self, bit: u32) {
        self.zeros = self.zeros + (self.history >> 63) as u32 - bit;
        self.history = self.history << 1 | u64::from(bit);
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u32 {
    u32::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
}

struct Range<'a> {
    input: &'a [u8],
    next: usize,
    range: u32,
    code: u32,
}

impl Range<'_> {
    fn normalize(&mut self) -> Field<()> {
        if self.range <= RANGE_NORMALIZE {
            field_check(
                self.next + 2 <= self.input.len(),
                "LZMS range coder runs past the end of its chunk",
            )?;
            self.range <<= 16;
            self.code = self.code << 16 | u16_at(self.input, self.next);
            self.next += 2;
        }
        Ok(())
    }

    fn bit(&mut self, state: &mut usize, probabilities: &mut [Probability]) -> Field<u32> {
        let entry = &mut probabilities[*state];
        let probability = entry.of_zero();
        self.normalize()?;
        let bound = (self.range >> PROBABILITY_BITS) * probability;
        let bit = if self.code < bound {
            self.range = bound;
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            1
        };
        entry.update(bit);
        *state = (*state << 1 | bit as usize) & (probabilities.len() - 1);
        Ok(bit)
    }

    fn repeat(
        &mut self,
        states: &mut [usize; REPEATS],
        probabilities: &mut Repeats,
    ) -> Field<usize> {
        let mut index = 0;
        while index + 1 < REPEATS
            && self.bit(&mut states[index + 1], &mut probabilities[index + 1])? == 1
        {
            index += 1;
        }
        Ok(index)
    }
}

struct Backward<'a> {
    input: &'a [u8],
    end: usize,
    used: u32,
}

impl Backward<'_> {
    fn byte_before(&self, back: usize) -> u64 {
        self.end
            .checked_sub(back)
            .map_or(0, |at| u64::from(self.input[at]))
    }

    fn peek(&self, count: u32) -> u32 {
        let window = (1..=5).fold(0u64, |window, back| window << 8 | self.byte_before(back));
        ((window << self.used) >> (40 - count) & ((1 << count) - 1)) as u32
    }

    fn consume(&mut self, count: u32, range: &Range) -> Field<()> {
        let used = self.used + count;
        let bytes = (used / 8) as usize;
        field_check(
            bytes <= self.end && self.end - bytes >= range.next,
            "LZMS bit stream runs into its range coder",
        )?;
        self.end -= bytes;
        self.used = used % 8;
        Ok(())
    }

    fn read(&mut self, count: u32, range: &Range) -> Field<u32> {
        if count == 0 {
            return Ok(0);
        }
        let value = self.peek(count);
        self.consume(count, range)?;
        Ok(value)
    }
}

// SPEC(wimlib compress_common): leaves sort by frequency then symbol, the tree merges the
// two lightest nodes preferring a leaf on a tie, and no code exceeds 15 bits.
fn lengths_from_frequencies(frequencies: &[u32], lengths: &mut [u8], work: &mut Work) {
    let count = frequencies.len();
    work.order.clear();
    work.order.extend(
        (0..count as u32).map(|symbol| frequencies[symbol as usize] << SYMBOL_BITS | symbol),
    );
    work.order.sort_unstable();
    let leaf = |index: usize| work.order[index] >> SYMBOL_BITS;
    work.weight.clear();
    work.weight.resize(count - 1, 0);
    work.parent.clear();
    work.parent.resize(count - 1, 0);
    let (mut leaves, mut joined) = (0, 0);
    for node in 0..count - 1 {
        let mut weight = 0;
        for _ in 0..2 {
            if leaves < count && (joined == node || leaf(leaves) <= work.weight[joined]) {
                weight += leaf(leaves);
                leaves += 1;
            } else {
                weight += work.weight[joined];
                work.parent[joined] = node as u32;
                joined += 1;
            }
        }
        work.weight[node] = weight;
    }
    let mut at_level = [0u32; LONGEST_CODE as usize + 1];
    at_level[1] = 2;
    let root = count - 2;
    work.depth.clear();
    work.depth.resize(count - 1, 0);
    for node in (0..root).rev() {
        let depth = work.depth[work.parent[node] as usize] + 1;
        work.depth[node] = depth;
        let mut level = depth as usize;
        if level > LAST_LEVEL_WITH_CHILDREN {
            level = LAST_LEVEL_WITH_CHILDREN;
            while level > 1 && at_level[level] == 0 {
                level -= 1;
            }
        }
        at_level[level] -= 1;
        at_level[level + 1] += 2;
    }
    let mut next = 0;
    for length in (1..=LONGEST_CODE as usize).rev() {
        for _ in 0..at_level[length] {
            lengths[(work.order[next] & ((1 << SYMBOL_BITS) - 1)) as usize] = length as u8;
            next += 1;
        }
    }
}

#[derive(Default)]
struct Work {
    order: Vec<u32>,
    weight: Vec<u32>,
    parent: Vec<u32>,
    depth: Vec<u8>,
}

struct Adaptive {
    huffman: Huffman,
    frequencies: Vec<u32>,
    lengths: Vec<u8>,
    rebuild_every: u32,
    until_rebuild: u32,
}

impl Adaptive {
    fn new(rebuild_every: u32) -> Self {
        Adaptive {
            huffman: Huffman::new(),
            frequencies: Vec::new(),
            lengths: Vec::new(),
            rebuild_every,
            until_rebuild: rebuild_every,
        }
    }

    fn reset(&mut self, symbols: usize, work: &mut Work) -> Field<()> {
        self.frequencies.clear();
        self.frequencies.resize(symbols, 1);
        self.lengths.clear();
        self.lengths.resize(symbols, 0);
        self.until_rebuild = self.rebuild_every;
        self.rebuild(work)
    }

    fn rebuild(&mut self, work: &mut Work) -> Field<()> {
        lengths_from_frequencies(&self.frequencies, &mut self.lengths, work);
        self.huffman.build(&self.lengths)
    }

    fn decode(&mut self, bits: &mut Backward, range: &Range, work: &mut Work) -> Field<usize> {
        let (symbol, length) = self.huffman.decode(bits.peek(LONGEST_CODE));
        bits.consume(length, range)?;
        self.frequencies[symbol] += 1;
        self.until_rebuild -= 1;
        if self.until_rebuild == 0 {
            self.rebuild(work)?;
            self.until_rebuild = self.rebuild_every;
            for frequency in &mut self.frequencies {
                *frequency = (*frequency >> 1) + 1;
            }
        }
        Ok(symbol)
    }
}

type Repeats = [[Probability; REPEAT_STATES]; REPEATS];

struct Models {
    main_state: usize,
    match_state: usize,
    lz_states: [usize; REPEATS],
    delta_states: [usize; REPEATS],
    main: [Probability; MAIN_STATES],
    matches: [Probability; MATCH_STATES],
    lz: Repeats,
    delta: Repeats,
}

impl Models {
    fn new() -> Self {
        Models {
            main_state: 0,
            match_state: 0,
            lz_states: [0; REPEATS],
            delta_states: [0; REPEATS],
            main: [Probability::INITIAL; MAIN_STATES],
            matches: [Probability::INITIAL; MATCH_STATES],
            lz: [[Probability::INITIAL; REPEAT_STATES]; REPEATS],
            delta: [[Probability::INITIAL; REPEAT_STATES]; REPEATS],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    Literal,
    Lz,
    Delta,
}

fn bring_forward<T: Copy>(recent: &mut [T; REPEATS + 1], index: usize) -> T {
    let value = recent[index];
    recent.copy_within(0..index, 1);
    recent[0] = value;
    value
}

fn push_front<T: Copy>(recent: &mut [T; REPEATS + 1], value: T) {
    recent.copy_within(0..REPEATS, 1);
    recent[0] = value;
}

pub(crate) struct Lzms {
    literal: Adaptive,
    position: Adaptive,
    length: Adaptive,
    power: Adaptive,
    delta: Adaptive,
    work: Work,
    x86_history: Vec<i32>,
}

impl Lzms {
    pub(crate) fn new() -> Self {
        Lzms {
            literal: Adaptive::new(LITERAL_REBUILD),
            position: Adaptive::new(POSITION_REBUILD),
            length: Adaptive::new(LENGTH_REBUILD),
            power: Adaptive::new(POWER_REBUILD),
            delta: Adaptive::new(POSITION_REBUILD),
            work: Work::default(),
            x86_history: vec![0; X86_HISTORY],
        }
    }

    pub(crate) fn decode(&mut self, input: &[u8], output: &mut [u8]) -> Field<()> {
        self.decode_symbols(input, output)?;
        x86_filter(output, &mut self.x86_history);
        Ok(())
    }

    fn length(&mut self, bits: &mut Backward, range: &Range) -> Field<usize> {
        let slot = self.length.decode(bits, range, &mut self.work)?;
        let extra = bits.read(u32::from(LENGTH_DIRECT_BITS[slot]), range)?;
        Ok((LENGTH_BASE[slot] + extra) as usize)
    }

    fn slot_distance(&mut self, delta: bool, bits: &mut Backward, range: &Range) -> Field<u32> {
        let model = if delta {
            &mut self.delta
        } else {
            &mut self.position
        };
        let slot = model.decode(bits, range, &mut self.work)?;
        let extra = bits.read(u32::from(POSITION_DIRECT_BITS[slot]), range)?;
        Ok(POSITION_BASE[slot] + extra)
    }

    // EXTERNAL(7-Zip LzmsDecoder): a repeated match right after a match of its own kind
    // takes the repeat one further back, since that match has not yet entered the list.
    fn decode_symbols(&mut self, input: &[u8], output: &mut [u8]) -> Field<()> {
        field_check(
            input.len() >= 8 && input.len().is_multiple_of(2),
            "LZMS chunk is shorter than 8 bytes or has an odd size",
        )?;
        let mut range = Range {
            input,
            next: 4,
            range: u32::MAX,
            code: u16_at(input, 0) << 16 | u16_at(input, 2),
        };
        field_check(
            range.code < range.range,
            "LZMS range coder starts out of range",
        )?;
        let mut bits = Backward {
            input,
            end: input.len(),
            used: 0,
        };
        let positions = position_symbols(output.len());
        self.literal.reset(LITERALS, &mut self.work)?;
        self.length.reset(LENGTH_SYMBOLS, &mut self.work)?;
        self.power.reset(POWER_SYMBOLS, &mut self.work)?;
        self.position.reset(positions, &mut self.work)?;
        self.delta.reset(positions, &mut self.work)?;
        let mut models = Models::new();
        let mut lz_recent: [u32; REPEATS + 1] = [1, 2, 3, 4];
        let mut delta_recent: [u64; REPEATS + 1] = [1, 2, 3, 4];
        let mut previous = Item::Literal;
        let mut at = 0;
        while at < output.len() {
            if range.bit(&mut models.main_state, &mut models.main)? == 0 {
                output[at] = self.literal.decode(&mut bits, &range, &mut self.work)? as u8;
                at += 1;
                previous = Item::Literal;
            } else if range.bit(&mut models.match_state, &mut models.matches)? == 0 {
                let distance = if range.bit(&mut models.lz_states[0], &mut models.lz[0])? == 0 {
                    let distance = self.slot_distance(false, &mut bits, &range)?;
                    push_front(&mut lz_recent, distance);
                    distance
                } else {
                    let index = range.repeat(&mut models.lz_states, &mut models.lz)?;
                    bring_forward(&mut lz_recent, index + usize::from(previous == Item::Lz))
                } as usize;
                let length = self.length(&mut bits, &range)?;
                field_check(
                    length <= output.len() - at,
                    "LZMS match runs past the end of its chunk",
                )?;
                field_check(
                    distance <= at,
                    "LZMS match reaches before the start of its chunk",
                )?;
                copy_match(output, at, distance, length);
                at += length;
                previous = Item::Lz;
            } else {
                let pair = if range.bit(&mut models.delta_states[0], &mut models.delta[0])? == 0 {
                    let power = self.power.decode(&mut bits, &range, &mut self.work)? as u64;
                    let raw = self.slot_distance(true, &mut bits, &range)?;
                    let pair = power << 32 | u64::from(raw);
                    push_front(&mut delta_recent, pair);
                    pair
                } else {
                    let index = range.repeat(&mut models.delta_states, &mut models.delta)?;
                    bring_forward(
                        &mut delta_recent,
                        index + usize::from(previous == Item::Delta),
                    )
                };
                let power = (pair >> 32) as u32;
                let distance = ((pair as u32) << power) as usize;
                let length = self.length(&mut bits, &range)?;
                field_check(
                    length <= output.len() - at,
                    "LZMS delta match runs past the end of its chunk",
                )?;
                let span = 1usize << power;
                field_check(
                    distance + span <= at,
                    "LZMS delta match reaches before the start of its chunk",
                )?;
                for offset in at..at + length {
                    output[offset] = output[offset - span]
                        .wrapping_add(output[offset - distance])
                        .wrapping_sub(output[offset - distance - span]);
                }
                at += length;
                previous = Item::Delta;
            }
        }
        range.normalize()?;
        field_check(range.code == 0, "LZMS range coder does not end at zero")?;
        field_check(
            range.next < bits.end || range.next == bits.end && bits.used == 0,
            "LZMS bit stream and range coder overlap",
        )
    }
}

fn is_x86_opcode(byte: u8) -> bool {
    matches!(byte, 0x48 | 0x4C | 0xE8 | 0xE9 | 0xF0 | 0xFF)
}

fn x86_operand_at(data: &[u8], at: usize) -> Option<(usize, i32)> {
    let [opcode, second, third] = [data[at], data[at + 1], data[at + 2]];
    let rip_relative = u32::from(third).wrapping_sub(5);
    match opcode {
        0x48 | 0x4C => {
            let lea = second == 0x8D;
            let mov = second == 0x8B && opcode == 0x48 && rip_relative & 0xF7 == 0;
            (rip_relative & 7 == 0 && (lea || mov)).then_some((3, X86_TRANSLATION_REACH))
        }
        0xE8 => Some((1, X86_TRANSLATION_REACH / 2)),
        0xF0 => (second == 0x83 && third == 0x05).then_some((3, X86_TRANSLATION_REACH)),
        0xFF => (second == 0x15).then_some((2, X86_TRANSLATION_REACH)),
        _ => None,
    }
}

// EXTERNAL(7-Zip LzmsDecoder): the x86 filter skips the first byte and the last 16, skips
// a jump's operand, and undoes a translation only near an earlier translated operand.
fn x86_filter(data: &mut [u8], history: &mut [i32]) {
    if data.len() <= X86_SHORTEST_FILTERED {
        return;
    }
    history.fill(-X86_WINDOW - 1);
    let limit = data.len() - X86_TAIL;
    let mut last_translated = -X86_TRANSLATION_REACH - 1;
    let mut at = 0;
    loop {
        at += 1;
        while at < limit && !is_x86_opcode(data[at]) {
            at += 1;
        }
        if at >= limit {
            return;
        }
        if data[at] == 0xE9 {
            at += X86_OPERAND;
            continue;
        }
        let Some((operand_at, reach)) = x86_operand_at(data, at) else {
            continue;
        };
        let position = at as i32;
        let operand = &mut data[at + operand_at..at + operand_at + X86_OPERAND];
        let mut value = u32::from_le_bytes(operand.try_into().unwrap());
        if position - last_translated <= reach {
            value = value.wrapping_sub(position as u32);
            operand.copy_from_slice(&value.to_le_bytes());
        }
        let target = (position as u32).wrapping_add(value) as usize & (X86_HISTORY - 1);
        at += operand_at + X86_OPERAND - 1;
        let end = at as i32;
        if end - history[target] <= X86_WINDOW {
            last_translated = end;
        }
        history[target] = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Random(u64);

    impl Random {
        fn below(&mut self, limit: u32) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % u64::from(limit)) as u32
        }
    }

    #[test]
    fn every_built_code_is_complete_and_no_longer_than_15_bits() {
        let mut random = Random(0x1357_9bdf_2468_ace0);
        let mut work = Work::default();
        let mut huffman = Huffman::new();
        for round in 0..3000 {
            let count = [2, 3, 8, 54, 256, 799][round % 6];
            let skew = 1 + random.below(12);
            let frequencies: Vec<u32> = (0..count)
                .map(|_| {
                    let draw = random.below(1 << skew);
                    if draw == 0 {
                        2049
                    } else {
                        1 + random.below((2048 >> skew) + 1)
                    }
                })
                .collect();
            let mut lengths = vec![0u8; count];
            lengths_from_frequencies(&frequencies, &mut lengths, &mut work);
            let space: u64 = lengths
                .iter()
                .map(|&length| {
                    assert!((1..=15).contains(&length), "{length}");
                    1u64 << (15 - length)
                })
                .sum();
            assert_eq!(space, 1 << 15, "round {round}");
            huffman.build(&lengths).unwrap();
        }
    }

    #[test]
    fn a_skewed_code_is_limited_by_moving_the_deepest_leaves_up() {
        let frequencies: Vec<u32> = (0..20).map(|index| 1 << index.min(19)).collect();
        let mut lengths = vec![0u8; 20];
        lengths_from_frequencies(&frequencies, &mut lengths, &mut Work::default());
        assert_eq!(lengths[19], 1);
        assert!(lengths.iter().all(|&length| length <= 15));
        let space: u32 = lengths.iter().map(|&length| 1 << (15 - length)).sum();
        assert_eq!(space, 1 << 15);
    }

    #[test]
    fn equal_weights_prefer_the_lower_symbol_for_the_shorter_code() {
        let mut lengths = vec![0u8; 3];
        lengths_from_frequencies(&[1, 1, 1], &mut lengths, &mut Work::default());
        assert_eq!(lengths, [2, 2, 1]);
        let mut lengths = vec![0u8; 2];
        lengths_from_frequencies(&[5, 1], &mut lengths, &mut Work::default());
        assert_eq!(lengths, [1, 1]);
    }

    #[test]
    fn position_tables_cover_every_chunk_size() {
        assert_eq!(POSITION_BASE[0], 1);
        assert_eq!(POSITION_DIRECT_BITS[POSITION_SYMBOLS - 1], 30);
        assert_eq!(position_symbols(0), 2);
        assert_eq!(position_symbols(2), 2);
        assert_eq!(position_symbols(10), 9);
        assert_eq!(position_symbols(usize::MAX), POSITION_SYMBOLS);
        assert_eq!(LENGTH_BASE[0], 1);
        let longest = LENGTH_BASE[LENGTH_SYMBOLS - 1] as u64 + (1 << 30) - 1;
        assert!(longest < u64::from(u32::MAX));
        let farthest = POSITION_BASE[POSITION_SYMBOLS - 1] as u64 + (1 << 30) - 1;
        assert!(farthest < u64::from(u32::MAX));
    }

    #[test]
    fn every_short_chunk_is_refused_or_decodes_without_panicking() {
        let mut lzms = Lzms::new();
        let mut random = Random(0xdead_beef_0bad_cafe);
        for _ in 0..20_000 {
            let length = 2 * (random.below(40) as usize);
            let input: Vec<u8> = (0..length).map(|_| random.below(256) as u8).collect();
            let mut output = vec![0; 1 + random.below(3000) as usize];
            let _ = lzms.decode(&input, &mut output);
        }
        let mut output = vec![0; 16];
        assert!(lzms.decode(&[0; 7], &mut output).is_err());
        assert!(lzms.decode(&[0; 9], &mut output).is_err());
        assert!(lzms.decode(&[0xFF; 8], &mut output).is_err());
    }

    fn reference_x86(data: &mut [u8]) {
        if data.len() <= 17 {
            return;
        }
        let mut history = vec![-65536i32; 65536];
        let size = data.len() - 16;
        let mut last: i32 = -1024;
        let mut i: usize = 0;
        'scan: loop {
            let mut p = i;
            loop {
                p += 1;
                if p >= size {
                    break 'scan;
                }
                if matches!(data[p], 0x48 | 0x4C | 0xE8 | 0xE9 | 0xF0 | 0xFF) {
                    break;
                }
            }
            i = p;
            let b = data[p];
            let mut reach = 1023;
            let code_length = if b == 0x48 || b == 0x4C {
                let b2 = (data[p + 2] as u32).wrapping_sub(5);
                if b2 & 7 != 0 {
                    continue;
                }
                if data[p + 1] != 0x8D && (data[p + 1] != 0x8B || b != 0x48 || b2 & 0xF7 != 0) {
                    continue;
                }
                3
            } else if b == 0xE8 {
                reach /= 2;
                1
            } else if b == 0xE9 {
                i += 4;
                continue;
            } else if b == 0xF0 {
                if data[p + 1] != 0x83 || data[p + 2] != 0x05 {
                    continue;
                }
                3
            } else {
                if data[p + 1] != 0x15 {
                    continue;
                }
                2
            };
            let at = p + code_length;
            let mut n = u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
            if i as i32 - last <= reach {
                n = (n as i32).wrapping_sub(i as i32) as u32;
                data[at..at + 4].copy_from_slice(&n.to_le_bytes());
            }
            let target = ((i as u32).wrapping_add(n) & 0xFFFF) as usize;
            i += code_length + 3;
            if i as i32 - history[target] <= 65535 {
                last = i as i32;
            }
            history[target] = i as i32;
        }
    }

    #[test]
    fn the_x86_filter_matches_a_byte_at_a_time_reference() {
        let mut random = Random(0x0123_4567_89ab_cdef);
        let mut history = vec![0; X86_HISTORY];
        let interesting = [
            0x48, 0x4C, 0xE8, 0xE9, 0xF0, 0xFF, 0x8D, 0x8B, 0x05, 0x0D, 0x83, 0x15, 0, 1,
        ];
        for round in 0..20_000 {
            let length = random.below(if round % 2 == 0 { 40 } else { 3000 }) as usize;
            let data: Vec<u8> = (0..length)
                .map(|_| {
                    if random.below(3) == 0 {
                        random.below(256) as u8
                    } else {
                        interesting[random.below(interesting.len() as u32) as usize]
                    }
                })
                .collect();
            let mut expected = data.clone();
            reference_x86(&mut expected);
            let mut filtered = data.clone();
            x86_filter(&mut filtered, &mut history);
            assert_eq!(filtered, expected, "round {round}");
        }
    }
}
