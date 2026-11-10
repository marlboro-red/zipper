use std::io::{self, BufRead, Read};

const MAGIC: u32 = 0xFD2F_B528;
const SKIPPABLE_MAGIC: u32 = 0x184D_2A50;
const SKIPPABLE_MASK: u32 = 0xFFFF_FFF0;
const BLOCK_MAX: usize = 128 * 1024;
const WINDOW_LOG_MAX: u32 = 31;
// EXTERNAL(libzstd 1.5.7): ZSTD_MAXWINDOWSIZE_DEFAULT, the default limit of ZSTD_decompressStream.
pub(crate) const WINDOW_LIMIT: u64 = (1 << 27) + 1;
const WILDCOPY: usize = 32;
const RING_SLACK: usize = 64;

const FRONT: usize = 8;
const INDEX_MASK: usize = (1 << 18) - 1;
const PADDED: usize = (1 << 18) + 32;
type Padded = [u8; PADDED];

const HUFFMAN_LOG_MAX: u32 = 12;
const HUFFMAN_FAST_LOG: u32 = 11;
const HUFFMAN_SLOTS: usize = 1 << HUFFMAN_LOG_MAX;
const WEIGHT_LOG_MAX: u32 = 6;
const SYMBOLS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Corrupt(pub(crate) &'static str);

type Checked<T> = Result<T, Corrupt>;

fn corrupt<T>(why: &'static str) -> Checked<T> {
    Err(Corrupt(why))
}

fn ensure(holds: bool, why: &'static str) -> Checked<()> {
    if holds { Ok(()) } else { Err(Corrupt(why)) }
}

const DAMAGED_DESCRIPTION: &str = "an FSE table description is damaged";
const DAMAGED_WEIGHTS: &str = "the Huffman weights are damaged";
const DAMAGED_TREE: &str = "the Huffman tree description is damaged";
const NOT_A_TREE: &str = "the Huffman weights do not form a tree";
const DAMAGED_TABLE: &str = "a sequence table is damaged";
pub(crate) const SIZE_DIFFERS: &str = "the frame's size differs from its header";
pub(crate) const BEYOND_WINDOW: &str = "a match reaches back beyond the frame's window";

impl From<Corrupt> for io::Error {
    fn from(error: Corrupt) -> Self {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Zstandard data is damaged: {}", error.0),
        )
    }
}

fn padded() -> Box<Padded> {
    vec![0u8; PADDED]
        .into_boxed_slice()
        .try_into()
        .unwrap_or_else(|_| unreachable!())
}

#[inline(always)]
fn le64(buffer: &Padded, at: usize) -> u64 {
    let at = at & INDEX_MASK;
    u64::from_le_bytes(buffer[at..at + 8].try_into().unwrap())
}

#[inline(always)]
fn byte(buffer: &Padded, at: usize) -> u8 {
    buffer[at & INDEX_MASK]
}

fn le16(buffer: &Padded, at: usize) -> usize {
    usize::from(byte(buffer, at)) | usize::from(byte(buffer, at + 1)) << 8
}

fn high_bit(value: u32) -> u32 {
    31 - (value | 1).leading_zeros()
}

const PRIMES: [u64; 5] = [
    0x9E37_79B1_85EB_CA87,
    0xC2B2_AE3D_27D4_EB4F,
    0x1656_67B1_9E37_79F9,
    0x85EB_CA77_C2B2_AE63,
    0x27D4_EB2F_1656_67C5,
];

// SPEC(xxHash specification, XXH64): four lanes over 32-byte stripes, then a merge and an avalanche.
pub(crate) struct Xxh64 {
    lanes: [u64; 4],
    held: [u8; 32],
    held_len: usize,
    total: u64,
}

impl Xxh64 {
    pub(crate) fn new() -> Self {
        Xxh64 {
            lanes: [
                PRIMES[0].wrapping_add(PRIMES[1]),
                PRIMES[1],
                0,
                0u64.wrapping_sub(PRIMES[0]),
            ],
            held: [0; 32],
            held_len: 0,
            total: 0,
        }
    }

    #[inline(always)]
    fn round(lane: u64, word: u64) -> u64 {
        lane.wrapping_add(word.wrapping_mul(PRIMES[1]))
            .rotate_left(31)
            .wrapping_mul(PRIMES[0])
    }

    #[inline(always)]
    fn stripe(lanes: &mut [u64; 4], stripe: &[u8; 32]) {
        for (lane, word) in lanes.iter_mut().zip(stripe.chunks_exact(8)) {
            *lane = Self::round(*lane, u64::from_le_bytes(word.try_into().unwrap()));
        }
    }

    pub(crate) fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u64;
        if self.held_len > 0 {
            let take = (32 - self.held_len).min(data.len());
            self.held[self.held_len..self.held_len + take].copy_from_slice(&data[..take]);
            self.held_len += take;
            data = &data[take..];
            if self.held_len < 32 {
                return;
            }
            let held = self.held;
            Self::stripe(&mut self.lanes, &held);
            self.held_len = 0;
        }
        let (stripes, rest) = data.as_chunks::<32>();
        let mut lanes = self.lanes;
        for stripe in stripes {
            Self::stripe(&mut lanes, stripe);
        }
        self.lanes = lanes;
        self.held[..rest.len()].copy_from_slice(rest);
        self.held_len = rest.len();
    }

    pub(crate) fn digest(&self) -> u64 {
        let mut value = if self.total >= 32 {
            let [a, b, c, d] = self.lanes;
            let mut merged = a
                .rotate_left(1)
                .wrapping_add(b.rotate_left(7))
                .wrapping_add(c.rotate_left(12))
                .wrapping_add(d.rotate_left(18));
            for lane in self.lanes {
                merged = (merged ^ Self::round(0, lane))
                    .wrapping_mul(PRIMES[0])
                    .wrapping_add(PRIMES[3]);
            }
            merged
        } else {
            PRIMES[4]
        };
        value = value.wrapping_add(self.total);
        let rest = &self.held[..self.held_len];
        let (words, rest) = rest.as_chunks::<8>();
        for word in words {
            value = (value ^ Self::round(0, u64::from_le_bytes(*word)))
                .rotate_left(27)
                .wrapping_mul(PRIMES[0])
                .wrapping_add(PRIMES[3]);
        }
        let (halves, rest) = rest.as_chunks::<4>();
        for half in halves {
            value = (value ^ u64::from(u32::from_le_bytes(*half)).wrapping_mul(PRIMES[0]))
                .rotate_left(23)
                .wrapping_mul(PRIMES[1])
                .wrapping_add(PRIMES[2]);
        }
        for &byte in rest {
            value = (value ^ u64::from(byte).wrapping_mul(PRIMES[4]))
                .rotate_left(11)
                .wrapping_mul(PRIMES[0]);
        }
        value ^= value >> 33;
        value = value.wrapping_mul(PRIMES[1]);
        value ^= value >> 29;
        value = value.wrapping_mul(PRIMES[2]);
        value ^ (value >> 32)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Refill {
    Unfinished,
    EndOfBuffer,
    Completed,
    Overflow,
}

// EXTERNAL(libzstd 1.5.7): BIT_DStream_t state for state, so a damaged stream reads the same bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BitD {
    container: u64,
    consumed: u32,
    at: usize,
    start: usize,
}

impl BitD {
    fn open(buffer: &Padded, start: usize, length: usize) -> Checked<Self> {
        ensure(length > 0, "a bitstream is empty")?;
        let last = byte(buffer, start + length - 1);
        ensure(last != 0, "a bitstream has no end mark")?;
        let padding = 8 - high_bit(u32::from(last));
        if length >= 8 {
            let at = start + length - 8;
            return Ok(BitD {
                container: le64(buffer, at),
                consumed: padding,
                at,
                start,
            });
        }
        let mut container = 0u64;
        for index in 0..length {
            container |= u64::from(byte(buffer, start + index)) << (8 * index);
        }
        Ok(BitD {
            container,
            consumed: padding + (8 - length as u32) * 8,
            at: start,
            start,
        })
    }

    #[inline(always)]
    fn peek_fast(&self, bits: u32) -> usize {
        ((self.container << (self.consumed & 63)) >> (64u32.wrapping_sub(bits) & 63)) as usize
    }

    #[inline(always)]
    fn peek(&self, bits: u32) -> usize {
        let from = 64u32.wrapping_sub(self.consumed).wrapping_sub(bits) & 63;
        ((self.container >> from) & ((1u64 << (bits & 31)) - 1)) as usize
    }

    #[inline(always)]
    fn skip(&mut self, bits: u32) {
        self.consumed = self.consumed.wrapping_add(bits);
    }

    #[inline(always)]
    fn read(&mut self, bits: u32) -> usize {
        let value = self.peek(bits);
        self.skip(bits);
        value
    }

    #[inline(always)]
    fn refill(&mut self, buffer: &Padded) -> Refill {
        let (status, moved) = self.step_back();
        if moved {
            self.container = le64(buffer, self.at);
        }
        status
    }

    #[inline(always)]
    fn step_back(&mut self) -> (Refill, bool) {
        if self.consumed > 64 {
            return (Refill::Overflow, false);
        }
        if self.at >= self.start + 8 {
            self.at -= (self.consumed >> 3) as usize;
            self.consumed &= 7;
            return (Refill::Unfinished, true);
        }
        if self.at == self.start {
            let status = if self.consumed < 64 {
                Refill::EndOfBuffer
            } else {
                Refill::Completed
            };
            return (status, false);
        }
        let mut bytes = (self.consumed >> 3) as usize;
        let mut status = Refill::Unfinished;
        if self.at - self.start < bytes {
            bytes = self.at - self.start;
            status = Refill::EndOfBuffer;
        }
        self.at -= bytes;
        self.consumed -= bytes as u32 * 8;
        (status, true)
    }

    fn ended(&self) -> bool {
        self.at == self.start && self.consumed == 64
    }
}

// EXTERNAL(libzstd 1.5.7): FSE_readNCount_body step for step, with its reads past the description.
fn read_ncount(
    counts: &mut [i16; SYMBOLS],
    max_symbol: usize,
    source: &[u8],
) -> Checked<(usize, u32, usize)> {
    if source.len() < 8 {
        let mut whole = [0u8; 8];
        whole[..source.len()].copy_from_slice(source);
        let (symbols, log, used) = read_ncount(counts, max_symbol, &whole)?;
        ensure(used <= source.len(), DAMAGED_DESCRIPTION)?;
        return Ok((symbols, log, used));
    }
    let word = |at: usize| -> Checked<u32> {
        match source.get(at..at.wrapping_add(4)) {
            Some(bytes) => Ok(u32::from_le_bytes(bytes.try_into().unwrap())),
            None => corrupt(DAMAGED_DESCRIPTION),
        }
    };
    let end = source.len();
    let limit = max_symbol + 1;
    counts[..limit.min(SYMBOLS)].fill(0);
    let mut at = 0usize;
    let mut stream = word(0)?;
    let mut bits = (stream & 0xF) as i32 + 5;
    ensure(bits <= 15, DAMAGED_DESCRIPTION)?;
    stream >>= 4;
    let mut bit_count = 4i32;
    let log = bits as u32;
    let mut remaining = (1i32 << bits) + 1;
    let mut threshold = 1i32 << bits;
    bits += 1;
    let mut symbol = 0usize;
    let mut previous_zero = false;
    loop {
        if previous_zero {
            let mut repeats = ((!stream | 0x8000_0000).trailing_zeros() >> 1) as usize;
            while repeats >= 12 {
                symbol += 3 * 12;
                if at + 7 <= end {
                    at += 3;
                } else {
                    bit_count -= 8 * (end as i32 - 7 - at as i32);
                    bit_count &= 31;
                    at = end - 4;
                }
                stream = word(at)? >> (bit_count & 31);
                repeats = ((!stream | 0x8000_0000).trailing_zeros() >> 1) as usize;
            }
            symbol += 3 * repeats;
            stream >>= 2 * repeats;
            bit_count += 2 * repeats as i32;
            symbol += (stream & 3) as usize;
            bit_count += 2;
            if symbol >= limit {
                break;
            }
            if at + 7 <= end || at + (bit_count >> 3) as usize + 4 <= end {
                at += (bit_count >> 3) as usize;
                bit_count &= 7;
            } else {
                bit_count -= 8 * (end as i32 - 4 - at as i32);
                bit_count &= 31;
                at = end - 4;
            }
            stream = word(at)? >> (bit_count & 31);
        }
        let max = (2 * threshold - 1) - remaining;
        let mut count;
        if (stream & (threshold as u32 - 1)) < max as u32 {
            count = (stream & (threshold as u32 - 1)) as i32;
            bit_count += bits - 1;
        } else {
            count = (stream & (2 * threshold as u32 - 1)) as i32;
            if count >= threshold {
                count -= max;
            }
            bit_count += bits;
        }
        count -= 1;
        if count >= 0 {
            remaining -= count;
        } else {
            remaining += count;
        }
        counts[symbol & (SYMBOLS - 1)] = count as i16;
        symbol += 1;
        previous_zero = count == 0;
        if remaining < threshold {
            if remaining <= 1 {
                break;
            }
            bits = high_bit(remaining as u32) as i32 + 1;
            threshold = 1 << (bits - 1);
        }
        if symbol >= limit {
            break;
        }
        if at + 7 <= end || at + (bit_count >> 3) as usize + 4 <= end {
            at += (bit_count >> 3) as usize;
            bit_count &= 7;
        } else {
            bit_count -= 8 * (end as i32 - 4 - at as i32);
            bit_count &= 31;
            at = end - 4;
        }
        stream = word(at)? >> (bit_count & 31);
    }
    ensure(
        remaining == 1 && symbol <= limit && bit_count <= 32,
        DAMAGED_DESCRIPTION,
    )?;
    at += ((bit_count + 7) >> 3) as usize;
    Ok((symbol - 1, log, at))
}

// SPEC(RFC 8878 4.1.1): symbols go to states a fixed step apart, with the less-than-one symbols at the top.
fn spread_symbols(
    slots: &mut [u8; 512],
    counts: &[i16; SYMBOLS],
    max_symbol: usize,
    log: u32,
) -> [u16; SYMBOLS] {
    let size = 1usize << log;
    let mask = size - 1;
    let mut next = [0u16; SYMBOLS];
    let mut high = size - 1;
    let listed = &counts[..=max_symbol.min(SYMBOLS - 1)];
    for (symbol, &count) in listed.iter().enumerate() {
        if count == -1 {
            slots[high & 511] = symbol as u8;
            high = high.wrapping_sub(1);
            next[symbol] = 1;
        } else {
            next[symbol] = count as u16;
        }
    }
    let step = (size >> 1) + (size >> 3) + 3;
    let mut position = 0usize;
    for (symbol, &count) in listed.iter().enumerate() {
        for _ in 0..count.max(0) {
            slots[position & 511] = symbol as u8;
            position = (position + step) & mask;
            while position > high {
                position = (position + step) & mask;
            }
        }
    }
    next
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct WeightState {
    next: u16,
    symbol: u8,
    bits: u8,
}

// SPEC(RFC 8878 4.1.1): a symbol's states, in order, read just enough bits to cover the table once.
fn state_step(position: u16, log: u32) -> (u16, u8) {
    let bits = log.saturating_sub(high_bit(u32::from(position)));
    let next = (u32::from(position) << bits).wrapping_sub(1 << log);
    (next as u16, bits as u8)
}

fn weight_states(
    counts: &[i16; SYMBOLS],
    max_symbol: usize,
    log: u32,
) -> [WeightState; 1 << WEIGHT_LOG_MAX] {
    let mut slots = [0u8; 512];
    let mut next = spread_symbols(&mut slots, counts, max_symbol, log);
    let mut table = [WeightState::default(); 1 << WEIGHT_LOG_MAX];
    for (state, entry) in table.iter_mut().enumerate().take(1 << log) {
        let symbol = slots[state];
        let (step, bits) = state_step(next[usize::from(symbol)], log);
        next[usize::from(symbol)] += 1;
        *entry = WeightState {
            next: step,
            symbol,
            bits,
        };
    }
    table
}

// EXTERNAL(libzstd 1.5.7): FSE_decompress_wksp_body and FSE_decompress_usingDTable_generic, which ends on the read that passes the stream's start.
fn fse_weights(
    weights: &mut [u8; SYMBOLS],
    buffer: &Padded,
    start: usize,
    length: usize,
) -> Checked<usize> {
    const CAPACITY: usize = SYMBOLS - 1;
    let from = start & INDEX_MASK;
    let source = &buffer[from..(from + length).min(PADDED)];
    let mut counts = [0i16; SYMBOLS];
    let (max_symbol, log, used) = read_ncount(&mut counts, SYMBOLS - 1, source)?;
    ensure(log <= WEIGHT_LOG_MAX, DAMAGED_WEIGHTS)?;
    let table = weight_states(&counts, max_symbol, log);
    let mut bit = BitD::open(buffer, start + used, length - used)?;
    let mut first = bit.read(log);
    bit.refill(buffer);
    let mut second = bit.read(log);
    bit.refill(buffer);
    ensure(bit.refill(buffer) != Refill::Overflow, DAMAGED_WEIGHTS)?;
    let step = |state: &mut usize, bit: &mut BitD| -> u8 {
        let entry = table[*state & ((1 << WEIGHT_LOG_MAX) - 1)];
        *state = usize::from(entry.next) + bit.read(u32::from(entry.bits));
        entry.symbol
    };
    let mut count = 0usize;
    while bit.refill(buffer) == Refill::Unfinished && count < CAPACITY - 3 {
        weights[count] = step(&mut first, &mut bit);
        weights[count + 1] = step(&mut second, &mut bit);
        weights[count + 2] = step(&mut first, &mut bit);
        weights[count + 3] = step(&mut second, &mut bit);
        count += 4;
    }
    loop {
        ensure(count <= CAPACITY - 2, DAMAGED_WEIGHTS)?;
        weights[count] = step(&mut first, &mut bit);
        count += 1;
        if bit.refill(buffer) == Refill::Overflow {
            weights[count] = step(&mut second, &mut bit);
            return Ok(count + 1);
        }
        ensure(count <= CAPACITY - 2, DAMAGED_WEIGHTS)?;
        weights[count] = step(&mut second, &mut bit);
        count += 1;
        if bit.refill(buffer) == Refill::Overflow {
            weights[count] = step(&mut first, &mut bit);
            return Ok(count + 1);
        }
    }
}

struct Weights {
    weights: [u8; SYMBOLS],
    symbols: usize,
    ranks: [u32; HUFFMAN_LOG_MAX as usize + 1],
    log: u32,
    used: usize,
}

// SPEC(RFC 8878 4.2.1): weights come four bits each or FSE coded, and the last one is implied.
fn read_weights(buffer: &Padded, start: usize, length: usize) -> Checked<Weights> {
    ensure(length > 0, DAMAGED_TREE)?;
    let header = usize::from(byte(buffer, start));
    let mut weights = [0u8; SYMBOLS];
    let (listed, used) = if header >= 128 {
        let listed = header - 127;
        let used = listed.div_ceil(2);
        ensure(used < length, DAMAGED_TREE)?;
        for (index, weight) in weights.iter_mut().enumerate().take(listed) {
            let pair = byte(buffer, start + 1 + index / 2);
            *weight = if index % 2 == 0 { pair >> 4 } else { pair & 15 };
        }
        (listed, used)
    } else {
        ensure(header < length, DAMAGED_TREE)?;
        (
            fse_weights(&mut weights, buffer, start + 1, header)?,
            header,
        )
    };
    weights_from_list(weights, listed, used + 1)
}

fn weights_from_list(mut weights: [u8; SYMBOLS], listed: usize, used: usize) -> Checked<Weights> {
    let mut ranks = [0u32; HUFFMAN_LOG_MAX as usize + 1];
    let mut total = 0u32;
    ensure(listed < SYMBOLS, NOT_A_TREE)?;
    for &weight in &weights[..listed] {
        ensure(u32::from(weight) <= HUFFMAN_LOG_MAX, NOT_A_TREE)?;
        ranks[usize::from(weight)] += 1;
        total += (1u32 << weight) >> 1;
    }
    ensure(total != 0, NOT_A_TREE)?;
    let log = high_bit(total) + 1;
    ensure(log <= HUFFMAN_LOG_MAX, NOT_A_TREE)?;
    let rest = (1u32 << log) - total;
    ensure(rest.is_power_of_two(), NOT_A_TREE)?;
    let last = high_bit(rest) + 1;
    weights[listed] = last as u8;
    ranks[last as usize] += 1;
    ensure(ranks[1] >= 2 && ranks[1] % 2 == 0, NOT_A_TREE)?;
    Ok(Weights {
        weights,
        symbols: listed + 1,
        ranks,
        log,
        used,
    })
}

// EXTERNAL(libzstd 1.5.7): HUF_readDTableX1_wksp, which widens a short table to 11 bits.
fn build_single(table: &mut [u16], weights: &Weights, fast_log: u32) -> u32 {
    let scale = fast_log.saturating_sub(weights.log);
    let log = weights.log + scale;
    let mut sorted = [0u8; SYMBOLS];
    let mut starts = [0usize; HUFFMAN_LOG_MAX as usize + 2];
    let mut next = 0usize;
    for (weight, start) in starts
        .iter_mut()
        .enumerate()
        .take(HUFFMAN_LOG_MAX as usize + 1)
    {
        *start = next;
        next += weights.ranks[weight] as usize;
    }
    for symbol in 0..weights.symbols {
        let weight = usize::from(weights.weights[symbol]);
        sorted[starts[weight] & (SYMBOLS - 1)] = symbol as u8;
        starts[weight] += 1;
    }
    let mut symbol = weights.ranks[0] as usize;
    let mut slot = 0usize;
    for weight in 1..=weights.log {
        let run = 1usize << (weight + scale - 1);
        let bits = (weights.log + 1 - weight) as u16;
        for _ in 0..weights.ranks[weight as usize] {
            let entry = u16::from(sorted[symbol & (SYMBOLS - 1)]) << 8 | bits;
            if let Some(slots) = table.get_mut(slot..slot + run) {
                slots.fill(entry);
            }
            symbol += 1;
            slot += run;
        }
    }
    log
}

const DOUBLE_LENGTH_SHIFT: u32 = 24;
const DOUBLE_BITS_SHIFT: u32 = 16;

fn double_entry(symbol: u8, bits: u32, first: u8, length: u32) -> u32 {
    let sequence = if length == 1 {
        u32::from(symbol)
    } else {
        u32::from(first) | u32::from(symbol) << 8
    };
    sequence | bits << DOUBLE_BITS_SHIFT | length << DOUBLE_LENGTH_SHIFT
}

struct DoubleLayout {
    sorted: [u8; SYMBOLS],
    starts: [usize; HUFFMAN_LOG_MAX as usize + 3],
    values: [[u32; HUFFMAN_LOG_MAX as usize + 1]; HUFFMAN_LOG_MAX as usize + 1],
    max_weight: u32,
    target: u32,
    baseline: u32,
}

impl DoubleLayout {
    fn weight_run(&self, table: &mut [u32], weight: u32, bits: u32, first: u8, length: u32) {
        let run = 1usize << (self.target.saturating_sub(bits) & 31);
        let symbols = &self.sorted[self.starts[weight as usize].min(SYMBOLS)
            ..self.starts[weight as usize + 1].min(SYMBOLS)];
        for (index, &symbol) in symbols.iter().enumerate() {
            if let Some(slots) = table.get_mut(index * run..(index + 1) * run) {
                slots.fill(double_entry(symbol, bits, first, length));
            }
        }
    }

    fn second_level(&self, table: &mut [u32], consumed: u32, min_weight: u32, first: u8) {
        let values = &self.values[consumed as usize];
        if min_weight > 1 {
            let skipped = values[min_weight as usize] as usize;
            if let Some(slots) = table.get_mut(..skipped) {
                slots.fill(double_entry(first, consumed, 0, 1));
            }
        }
        for weight in min_weight..=self.max_weight {
            let from = values[weight as usize] as usize;
            if let Some(slots) = table.get_mut(from..) {
                self.weight_run(slots, weight, self.baseline - weight + consumed, first, 2);
            }
        }
    }
}

// EXTERNAL(libzstd 1.5.7): HUF_readDTableX2_wksp, whose entries hold two symbols where both fit.
fn build_double(table: &mut [u32], weights: &Weights, fast_log: u32) -> u32 {
    let log = weights.log;
    let target = log.max(fast_log);
    let mut max_weight = log;
    while max_weight > 1 && weights.ranks[max_weight as usize] == 0 {
        max_weight -= 1;
    }
    let mut layout = DoubleLayout {
        sorted: [0; SYMBOLS],
        starts: [0; HUFFMAN_LOG_MAX as usize + 3],
        values: [[0; HUFFMAN_LOG_MAX as usize + 1]; HUFFMAN_LOG_MAX as usize + 1],
        max_weight,
        target,
        baseline: log + 1,
    };
    let mut next = 0usize;
    for weight in 1..=max_weight as usize {
        layout.starts[weight] = next;
        next += weights.ranks[weight] as usize;
    }
    layout.starts[max_weight as usize + 1] = next;
    let mut cursor = layout.starts;
    cursor[0] = next;
    for symbol in 0..weights.symbols {
        let weight = usize::from(weights.weights[symbol]);
        layout.sorted[cursor[weight] & (SYMBOLS - 1)] = symbol as u8;
        cursor[weight] += 1;
    }
    let rescale = target - log;
    let mut value = 0u32;
    for weight in 1..=max_weight {
        layout.values[0][weight as usize] = value;
        value += (weights.ranks[weight as usize] << (weight + rescale)) >> 1;
    }
    let min_bits = log + 1 - max_weight;
    for consumed in min_bits..=target - min_bits {
        for weight in 1..=max_weight as usize {
            layout.values[consumed as usize][weight] = layout.values[0][weight] >> consumed;
        }
    }
    let scale = (log + 1) as i32 - target as i32;
    for weight in 1..=max_weight {
        let bits = log + 1 - weight;
        let from = layout.values[0][weight as usize] as usize;
        if target - bits >= min_bits {
            let run = 1usize << (target - bits);
            let min_weight = (bits as i32 + scale).max(1) as u32;
            let symbols = layout.starts[weight as usize]..layout.starts[weight as usize + 1];
            for (index, at) in symbols.enumerate() {
                let first = layout.sorted[at & (SYMBOLS - 1)];
                if let Some(slots) = table.get_mut(from + index * run..from + (index + 1) * run) {
                    layout.second_level(slots, bits, min_weight, first);
                }
            }
        } else if let Some(slots) = table.get_mut(from..) {
            layout.weight_run(slots, weight, bits, 0, 1);
        }
    }
    target
}

// EXTERNAL(libzstd 1.5.7): HUF_selectDecoder and its algoTime table, which pick the table a damaged stream is read with.
fn selects_double(decoded: usize, encoded: usize) -> bool {
    const TIMES: [[(u32, u32); 2]; 16] = [
        [(0, 0), (1, 1)],
        [(0, 0), (1, 1)],
        [(150, 216), (381, 119)],
        [(170, 205), (514, 112)],
        [(177, 199), (539, 110)],
        [(197, 194), (644, 107)],
        [(221, 192), (735, 107)],
        [(256, 189), (881, 106)],
        [(359, 188), (1167, 109)],
        [(582, 187), (1570, 114)],
        [(688, 187), (1712, 122)],
        [(825, 186), (1965, 136)],
        [(976, 185), (2131, 150)],
        [(1180, 186), (2070, 175)],
        [(1377, 185), (1731, 202)],
        [(1412, 185), (1695, 202)],
    ];
    let quantum = if encoded >= decoded {
        15
    } else {
        encoded * 16 / decoded
    };
    let per_256 = (decoded >> 8) as u32;
    let [single, double] = TIMES[quantum & 15];
    let single = single.0 + single.1 * per_256;
    let double = double.0 + double.1 * per_256;
    double + (double >> 5) < single
}

struct Huffman {
    single: [u16; HUFFMAN_SLOTS],
    double: [u32; HUFFMAN_SLOTS],
    log: u32,
    is_double: bool,
    ready: bool,
}

#[inline(always)]
fn single_symbol(
    output: &mut Padded,
    at: &mut usize,
    bit: &mut BitD,
    table: &[u16; HUFFMAN_SLOTS],
    log: u32,
) {
    let entry = table[bit.peek_fast(log) & (HUFFMAN_SLOTS - 1)];
    output[*at & INDEX_MASK] = (entry >> 8) as u8;
    bit.skip(u32::from(entry & 0xFF));
    *at += 1;
}

// EXTERNAL(libzstd 1.5.7): HUF_decodeStreamX1 with its refill schedule.
fn single_stream(
    output: &mut Padded,
    mut at: usize,
    end: usize,
    bit: &mut BitD,
    input: &Padded,
    table: &[u16; HUFFMAN_SLOTS],
    log: u32,
) {
    if end - at > 3 {
        while bit.refill(input) == Refill::Unfinished && at < end - 3 {
            for _ in 0..4 {
                single_symbol(output, &mut at, bit, table, log);
            }
        }
    } else {
        bit.refill(input);
    }
    while at < end {
        single_symbol(output, &mut at, bit, table, log);
    }
}

#[inline(always)]
fn double_symbol(
    output: &mut Padded,
    at: &mut usize,
    bit: &mut BitD,
    table: &[u32; HUFFMAN_SLOTS],
    log: u32,
) {
    let entry = table[bit.peek_fast(log) & (HUFFMAN_SLOTS - 1)];
    let to = *at & INDEX_MASK;
    output[to..to + 2].copy_from_slice(&(entry as u16).to_le_bytes());
    bit.skip((entry >> DOUBLE_BITS_SHIFT) & 0xFF);
    *at += (entry >> DOUBLE_LENGTH_SHIFT) as usize;
}

// EXTERNAL(libzstd 1.5.7): HUF_decodeStreamX2, and HUF_decodeLastSymbolX2 with its clamp on the last symbol.
fn double_stream(
    output: &mut Padded,
    mut at: usize,
    end: usize,
    bit: &mut BitD,
    input: &Padded,
    table: &[u32; HUFFMAN_SLOTS],
    log: u32,
) {
    if end - at >= 8 {
        let (lookups, reach) = if log <= 11 { (5, 9) } else { (4, 7) };
        while bit.refill(input) == Refill::Unfinished && at + reach < end {
            for _ in 0..lookups {
                double_symbol(output, &mut at, bit, table, log);
            }
        }
    } else {
        bit.refill(input);
    }
    if end - at >= 2 {
        while bit.refill(input) == Refill::Unfinished && at + 2 <= end {
            double_symbol(output, &mut at, bit, table, log);
        }
        while at + 2 <= end {
            double_symbol(output, &mut at, bit, table, log);
        }
    }
    if at < end {
        let entry = table[bit.peek_fast(log) & (HUFFMAN_SLOTS - 1)];
        output[at & INDEX_MASK] = entry as u8;
        let bits = (entry >> DOUBLE_BITS_SHIFT) & 0xFF;
        if entry >> DOUBLE_LENGTH_SHIFT == 1 {
            bit.skip(bits);
        } else if bit.consumed < 64 {
            bit.skip(bits);
            bit.consumed = bit.consumed.min(64);
        }
    }
}

struct FourStreams {
    starts: [usize; 4],
    lengths: [usize; 4],
    segment: usize,
}

const DAMAGED_LITERALS: &str = "the Huffman-coded literals are damaged";

fn four_streams(
    input: &Padded,
    start: usize,
    encoded: usize,
    decoded: usize,
) -> Checked<FourStreams> {
    ensure(encoded >= 10, DAMAGED_LITERALS)?;
    let first = le16(input, start);
    let second = le16(input, start + 2);
    let third = le16(input, start + 4);
    let fourth = encoded.wrapping_sub(first + second + third + 6);
    let starts = [
        start + 6,
        start + 6 + first,
        start + 6 + first + second,
        start + 6 + first + second + third,
    ];
    Ok(FourStreams {
        starts,
        lengths: [first, second, third, fourth],
        segment: decoded.div_ceil(4),
    })
}

struct FastState {
    bits: [u64; 4],
    at: [usize; 4],
    to: [usize; 4],
}

impl FourStreams {
    // EXTERNAL(libzstd 1.5.7): HUF_DecompressFastArgs_init, which neither needs an end mark nor checks one.
    fn fast_state(
        &self,
        input: &Padded,
        encoded: usize,
        decoded: usize,
        log: u32,
    ) -> Checked<Option<FastState>> {
        if log != HUFFMAN_FAST_LOG || self.lengths.iter().any(|&length| length < 8) {
            return Ok(None);
        }
        ensure(self.lengths[3] <= encoded, DAMAGED_LITERALS)?;
        if 3 * self.segment >= decoded {
            return Ok(None);
        }
        let mut state = FastState {
            bits: [0; 4],
            at: [0; 4],
            to: [0, self.segment, 2 * self.segment, 3 * self.segment],
        };
        for stream in 0..4 {
            let at = self.starts[stream] + self.lengths[stream] - 8;
            let last = byte(input, at + 7);
            let padding = if last == 0 {
                0
            } else {
                8 - high_bit(u32::from(last))
            };
            state.at[stream] = at;
            state.bits[stream] = (le64(input, at) | 1) << padding;
        }
        Ok(Some(state))
    }

    fn segment_end(&self, stream: usize, decoded: usize) -> usize {
        ((stream + 1) * self.segment).min(decoded)
    }

    // EXTERNAL(libzstd 1.5.7): HUF_initRemainingDStream, which lets a stream run on into the one before it.
    fn rest_of(
        &self,
        state: &FastState,
        stream: usize,
        lowest: usize,
        input: &Padded,
        decoded: usize,
    ) -> Checked<(BitD, usize)> {
        let end = self.segment_end(stream, decoded);
        ensure(
            state.to[stream] <= end && state.at[stream] + 8 >= self.starts[stream],
            DAMAGED_LITERALS,
        )?;
        let bit = BitD {
            container: le64(input, state.at[stream]),
            consumed: state.bits[stream].trailing_zeros(),
            at: state.at[stream],
            start: lowest,
        };
        Ok((bit, end))
    }
}

#[inline(always)]
fn reload(input: &Padded, bits: &mut u64, at: &mut usize) {
    let zeros = bits.trailing_zeros();
    *at = at.wrapping_sub((zeros >> 3) as usize);
    *bits = (le64(input, *at) | 1) << (zeros & 7);
}

fn crossed(at: &[usize; 4]) -> bool {
    at[1] < at[0] || at[2] < at[1] || at[3] < at[2]
}

// EXTERNAL(libzstd 1.5.7): HUF_decompress4X1_usingDTable_internal_fast_c_loop, batch for batch.
fn single_fast_loop(
    output: &mut Padded,
    input: &Padded,
    table: &[u16; 1 << HUFFMAN_FAST_LOG],
    state: &mut FastState,
    lowest: usize,
    decoded: usize,
) {
    let FastState {
        mut bits,
        mut at,
        mut to,
    } = *state;
    loop {
        let rounds = ((decoded - to[3]) / 5).min(at[0].wrapping_sub(lowest) / 7);
        if rounds == 0 || crossed(&at) {
            break;
        }
        for _ in 0..rounds {
            for symbol in 0..5 {
                for stream in 0..4 {
                    let entry = table[(bits[stream] >> 53) as usize];
                    bits[stream] <<= entry & 0x3F;
                    output[(to[stream] + symbol) & INDEX_MASK] = (entry >> 8) as u8;
                }
            }
            for stream in 0..4 {
                to[stream] += 5;
                reload(input, &mut bits[stream], &mut at[stream]);
            }
        }
    }
    *state = FastState { bits, at, to };
}

// EXTERNAL(libzstd 1.5.7): HUF_decompress4X2_usingDTable_internal_fast_c_loop, batch for batch.
fn double_fast_loop(
    output: &mut Padded,
    input: &Padded,
    table: &[u32; 1 << HUFFMAN_FAST_LOG],
    state: &mut FastState,
    lowest: usize,
    ends: [usize; 4],
) {
    let FastState {
        mut bits,
        mut at,
        mut to,
    } = *state;
    loop {
        let mut rounds = at[0].wrapping_sub(lowest) / 7;
        for stream in 0..4 {
            rounds = rounds.min((ends[stream] - to[stream]) / 10);
        }
        let limit = to[3] + rounds * 5;
        if rounds == 0 || crossed(&at) {
            break;
        }
        loop {
            for stream in 0..4 {
                for _ in 0..5 {
                    let entry = table[(bits[stream] >> 53) as usize];
                    let target = to[stream] & INDEX_MASK;
                    output[target..target + 2].copy_from_slice(&(entry as u16).to_le_bytes());
                    bits[stream] <<= (entry >> DOUBLE_BITS_SHIFT) & 0x3F;
                    to[stream] += (entry >> DOUBLE_LENGTH_SHIFT) as usize;
                }
                reload(input, &mut bits[stream], &mut at[stream]);
            }
            if to[3] >= limit {
                break;
            }
        }
    }
    *state = FastState { bits, at, to };
}

impl Huffman {
    fn new() -> Self {
        Huffman {
            single: [0; HUFFMAN_SLOTS],
            double: [0; HUFFMAN_SLOTS],
            log: 0,
            is_double: false,
            ready: false,
        }
    }

    fn rebuild(&mut self, weights: &Weights, double: bool) {
        self.log = if double {
            build_double(&mut self.double, weights, HUFFMAN_FAST_LOG)
        } else {
            build_single(&mut self.single, weights, HUFFMAN_FAST_LOG)
        };
        self.is_double = double;
        self.ready = true;
    }

    fn stream(&self, output: &mut Padded, at: usize, end: usize, bit: &mut BitD, input: &Padded) {
        if self.is_double {
            double_stream(output, at, end, bit, input, &self.double, self.log);
        } else {
            single_stream(output, at, end, bit, input, &self.single, self.log);
        }
    }

    fn one_stream(
        &self,
        output: &mut Padded,
        decoded: usize,
        input: &Padded,
        start: usize,
        encoded: usize,
    ) -> Checked<()> {
        let mut bit = BitD::open(input, start, encoded)?;
        self.stream(output, 0, decoded, &mut bit, input);
        ensure(bit.ended(), DAMAGED_LITERALS)
    }

    fn four(
        &self,
        output: &mut Padded,
        decoded: usize,
        input: &Padded,
        start: usize,
        encoded: usize,
    ) -> Checked<()> {
        let streams = four_streams(input, start, encoded, decoded)?;
        if let Some(mut state) = streams.fast_state(input, encoded, decoded, self.log)? {
            if self.is_double {
                let table = self.double.first_chunk().unwrap_or_else(|| unreachable!());
                let ends = [state.to[1], state.to[2], state.to[3], decoded];
                double_fast_loop(output, input, table, &mut state, start, ends);
            } else {
                let table = self.single.first_chunk().unwrap_or_else(|| unreachable!());
                single_fast_loop(output, input, table, &mut state, start, decoded);
            }
            for stream in 0..4 {
                let (mut bit, end) = streams.rest_of(&state, stream, start, input, decoded)?;
                self.stream(output, state.to[stream], end, &mut bit, input);
            }
            return Ok(());
        }
        ensure(
            decoded >= 6 && streams.lengths[3] <= encoded,
            DAMAGED_LITERALS,
        )?;
        let mut ended = true;
        for stream in 0..4 {
            let mut bit = BitD::open(input, streams.starts[stream], streams.lengths[stream])?;
            let from = stream * streams.segment;
            self.stream(
                output,
                from,
                streams.segment_end(stream, decoded),
                &mut bit,
                input,
            );
            ended &= bit.ended();
        }
        ensure(ended, DAMAGED_LITERALS)
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct SequenceState(u64);

impl SequenceState {
    fn new(base: u32, next: u16, extra: u8, bits: u8) -> Self {
        SequenceState(
            u64::from(base)
                | u64::from(next) << 32
                | u64::from(extra) << 48
                | u64::from(bits) << 56,
        )
    }

    #[inline(always)]
    fn base(self) -> usize {
        (self.0 & 0xFFFF_FFFF) as usize
    }

    #[inline(always)]
    fn next(self) -> usize {
        ((self.0 >> 32) & 0xFFFF) as usize
    }

    #[inline(always)]
    fn extra(self) -> u32 {
        ((self.0 >> 48) & 0xFF) as u32
    }

    #[inline(always)]
    fn bits(self) -> u32 {
        (self.0 >> 56) as u32
    }
}

struct Code {
    max_symbol: usize,
    max_log: u32,
    default_log: u32,
    default_counts: &'static [i16],
    bases: &'static [u32],
    extras: &'static [u8],
}

const LITERAL_LENGTHS: Code = Code {
    max_symbol: 35,
    max_log: 9,
    default_log: 6,
    default_counts: &[
        4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1,
        1, 1, -1, -1, -1, -1,
    ],
    bases: &[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48,
        64, 0x80, 0x100, 0x200, 0x400, 0x800, 0x1000, 0x2000, 0x4000, 0x8000, 0x10000,
    ],
    extras: &[
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10,
        11, 12, 13, 14, 15, 16,
    ],
};

const MATCH_LENGTHS: Code = Code {
    max_symbol: 52,
    max_log: 9,
    default_log: 6,
    default_counts: &[
        1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
    ],
    bases: &[
        3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
        27, 28, 29, 30, 31, 32, 33, 34, 35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 0x83, 0x103,
        0x203, 0x403, 0x803, 0x1003, 0x2003, 0x4003, 0x8003, 0x10003,
    ],
    extras: &[
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    ],
};

const OFFSETS: Code = Code {
    max_symbol: 31,
    max_log: 8,
    default_log: 5,
    default_counts: &[
        1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
    ],
    bases: &[
        0, 1, 1, 5, 0xD, 0x1D, 0x3D, 0x7D, 0xFD, 0x1FD, 0x3FD, 0x7FD, 0xFFD, 0x1FFD, 0x3FFD,
        0x7FFD, 0xFFFD, 0x1FFFD, 0x3FFFD, 0x7FFFD, 0xFFFFD, 0x1FFFFD, 0x3FFFFD, 0x7FFFFD, 0xFFFFFD,
        0x1FFFFFD, 0x3FFFFFD, 0x7FFFFFD, 0xFFFFFFD, 0x1FFFFFFD, 0x3FFFFFFD, 0x7FFFFFFD,
    ],
    extras: &[
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31,
    ],
};

struct SequenceTable<const N: usize> {
    states: [SequenceState; N],
    log: u32,
}

impl<const N: usize> SequenceTable<N> {
    fn new() -> Self {
        SequenceTable {
            states: [SequenceState::default(); N],
            log: 0,
        }
    }

    fn build(&mut self, code: &Code, counts: &[i16; SYMBOLS], max_symbol: usize, log: u32) {
        let mut slots = [0u8; 512];
        let mut next = spread_symbols(&mut slots, counts, max_symbol, log);
        for (state, entry) in self.states.iter_mut().enumerate().take(1 << log) {
            let symbol = usize::from(slots[state & 511]).min(code.max_symbol);
            let (step, bits) = state_step(next[symbol], log);
            next[symbol] += 1;
            *entry = SequenceState::new(code.bases[symbol], step, code.extras[symbol], bits);
        }
        self.log = log;
    }

    fn predefined(&mut self, code: &Code) {
        let mut counts = [0i16; SYMBOLS];
        counts[..code.default_counts.len()].copy_from_slice(code.default_counts);
        self.build(
            code,
            &counts,
            code.max_symbol.min(code.default_counts.len() - 1),
            code.default_log,
        );
    }

    // SPEC(RFC 8878 3.1.1.3.2.1): each table is predefined, one symbol, FSE coded or the last one used.
    fn select(&mut self, code: &Code, mode: u8, source: &[u8], repeatable: bool) -> Checked<usize> {
        match mode {
            0 => {
                self.predefined(code);
                Ok(0)
            }
            1 => {
                let symbol = usize::from(*source.first().ok_or(Corrupt(DAMAGED_TABLE))?);
                ensure(symbol <= code.max_symbol, DAMAGED_TABLE)?;
                self.states[0] = SequenceState::new(code.bases[symbol], 0, code.extras[symbol], 0);
                self.log = 0;
                Ok(1)
            }
            2 => {
                let mut counts = [0i16; SYMBOLS];
                let (max_symbol, log, used) = read_ncount(&mut counts, code.max_symbol, source)
                    .map_err(|_| Corrupt(DAMAGED_TABLE))?;
                ensure(log <= code.max_log, DAMAGED_TABLE)?;
                self.build(code, &counts, max_symbol, log);
                Ok(used)
            }
            _ => {
                ensure(repeatable, DAMAGED_TABLE)?;
                Ok(0)
            }
        }
    }
}

struct Literals {
    in_block: bool,
    from: usize,
    length: usize,
    referenced: bool,
}

struct Target<'a> {
    output: &'a mut [u8],
    at: usize,
    earlier: usize,
    capacity_end: usize,
    block_max: usize,
    window: usize,
    one_shot: bool,
}

thread_local! {
    static SPARE_DECODER: std::cell::Cell<Option<Box<Decoder>>> = const { std::cell::Cell::new(None) };
}

struct LeasedDecoder(Option<Box<Decoder>>);

impl LeasedDecoder {
    fn held(&mut self) -> &mut Decoder {
        self.0.get_or_insert_with(|| {
            SPARE_DECODER
                .try_with(std::cell::Cell::take)
                .ok()
                .flatten()
                .unwrap_or_else(|| Box::new(Decoder::new()))
        })
    }
}

impl Drop for LeasedDecoder {
    fn drop(&mut self) {
        if let Some(decoder) = self.0.take() {
            let _ = SPARE_DECODER.try_with(|spare| spare.set(Some(decoder)));
        }
    }
}

struct Decoder {
    block: Box<Padded>,
    literals: Box<Padded>,
    huffman: Box<Huffman>,
    literal_lengths: SequenceTable<512>,
    offsets: SequenceTable<256>,
    match_lengths: SequenceTable<512>,
    sequences_ready: bool,
    repeats: [usize; 3],
}

const DAMAGED_BLOCK: &str = "a compressed block is damaged";
const PAST_SIZE: &str = "a block decodes to more than its frame allows";

impl Decoder {
    fn new() -> Self {
        Decoder {
            block: padded(),
            literals: padded(),
            huffman: Box::new(Huffman::new()),
            literal_lengths: SequenceTable::new(),
            offsets: SequenceTable::new(),
            match_lengths: SequenceTable::new(),
            sequences_ready: false,
            repeats: [1, 4, 8],
        }
    }

    fn start_frame(&mut self) {
        self.huffman.ready = false;
        self.sequences_ready = false;
        self.repeats = [1, 4, 8];
    }

    // SPEC(RFC 8878 3.1.1.3.1): literals are raw, one repeated byte, or Huffman coded with a new or the last tree.
    fn read_literals(
        &mut self,
        size: usize,
        block_max: usize,
        room: usize,
    ) -> Checked<(Literals, usize)> {
        ensure(size >= 2, DAMAGED_BLOCK)?;
        let block = &*self.block;
        let first = byte(block, FRONT);
        let format = (first >> 2) & 3;
        let header = usize::from(first)
            | usize::from(byte(block, FRONT + 1)) << 8
            | usize::from(byte(block, FRONT + 2)) << 16
            | usize::from(byte(block, FRONT + 3)) << 24;
        let kind = first & 3;
        if kind < 2 {
            let (header_size, length) = match format {
                1 => (2, (header & 0xFFFF) >> 4),
                3 => (3, (header & 0xFF_FFFF) >> 4),
                _ => (1, usize::from(first >> 3)),
            };
            ensure(
                format == 0 || format == 2 || size > header_size - kind as usize,
                DAMAGED_BLOCK,
            )?;
            ensure(length <= block_max, DAMAGED_BLOCK)?;
            ensure(length <= room, PAST_SIZE)?;
            if kind == 0 {
                ensure(header_size + length <= size, DAMAGED_BLOCK)?;
                let literals = Literals {
                    in_block: true,
                    from: FRONT + header_size,
                    length,
                    referenced: header_size + length + WILDCOPY <= size,
                };
                return Ok((literals, header_size + length));
            }
            let repeated = byte(block, FRONT + header_size);
            self.literals[..length].fill(repeated);
            let literals = Literals {
                in_block: false,
                from: 0,
                length,
                referenced: false,
            };
            return Ok((literals, header_size + 1));
        }
        ensure(size >= 5, DAMAGED_BLOCK)?;
        let (single, header_size, length, encoded) = match format {
            0 | 1 => (
                format == 0,
                3,
                (header >> 4) & 0x3FF,
                (header >> 14) & 0x3FF,
            ),
            2 => (false, 4, (header >> 4) & 0x3FFF, header >> 18),
            _ => (
                false,
                5,
                (header >> 4) & 0x3_FFFF,
                (header >> 22) + (usize::from(byte(block, FRONT + 4)) << 10),
            ),
        };
        ensure(length <= block_max, DAMAGED_BLOCK)?;
        ensure(single || length >= 6, DAMAGED_BLOCK)?;
        ensure(encoded + header_size <= size, DAMAGED_BLOCK)?;
        ensure(length <= room, PAST_SIZE)?;
        let mut start = FRONT + header_size;
        let mut left = encoded;
        if kind == 3 {
            ensure(self.huffman.ready, DAMAGED_BLOCK)?;
        } else {
            ensure(single || left > 0, DAMAGED_LITERALS)?;
            let double = !single && selects_double(length, left);
            let weights = read_weights(block, start, left)?;
            self.huffman.rebuild(&weights, double);
            ensure(weights.used < left, DAMAGED_LITERALS)?;
            start += weights.used;
            left -= weights.used;
        }
        if single {
            self.huffman
                .one_stream(&mut self.literals, length, block, start, left)?;
        } else {
            self.huffman
                .four(&mut self.literals, length, block, start, left)?;
        }
        let literals = Literals {
            in_block: false,
            from: 0,
            length,
            referenced: false,
        };
        Ok((literals, header_size + encoded))
    }

    // SPEC(RFC 8878 3.1.1.3.2.1): a count of sequences, then one mode byte and the three tables.
    fn read_sequence_header(&mut self, from: usize, size: usize) -> Checked<(usize, usize)> {
        let block = &*self.block;
        let end = FRONT + size;
        let mut at = FRONT + from;
        ensure(at < end, DAMAGED_BLOCK)?;
        let mut count = usize::from(byte(block, at));
        at += 1;
        if count > 0x7F {
            if count == 0xFF {
                ensure(at + 2 <= end, DAMAGED_BLOCK)?;
                count = le16(block, at) + 0x7F00;
                at += 2;
            } else {
                ensure(at < end, DAMAGED_BLOCK)?;
                count = ((count - 0x80) << 8) + usize::from(byte(block, at));
                at += 1;
            }
        }
        if count == 0 {
            ensure(at == end, DAMAGED_BLOCK)?;
            return Ok((0, at));
        }
        ensure(at < end, DAMAGED_BLOCK)?;
        let modes = byte(block, at);
        ensure(modes & 3 == 0, DAMAGED_BLOCK)?;
        at += 1;
        let ready = self.sequences_ready;
        at += self
            .literal_lengths
            .select(&LITERAL_LENGTHS, modes >> 6, &block[at..end], ready)?;
        at += self
            .offsets
            .select(&OFFSETS, (modes >> 4) & 3, &block[at.min(end)..end], ready)?;
        at += self.match_lengths.select(
            &MATCH_LENGTHS,
            (modes >> 2) & 3,
            &block[at.min(end)..end],
            ready,
        )?;
        ensure(at <= end, DAMAGED_BLOCK)?;
        Ok((count, at))
    }

    fn compressed_block(&mut self, size: usize, target: &mut Target) -> Checked<()> {
        ensure(size <= target.block_max, DAMAGED_BLOCK)?;
        let capacity = target.capacity_end - target.at;
        let (found, used) =
            self.read_literals(size, target.block_max, target.block_max.min(capacity))?;
        let (count, at) = self.read_sequence_header(used, size)?;
        // EXTERNAL(libzstd 1.5.7): ZSTD_allocateLiteralsBuffer keeps literals 32 bytes past the largest block when the output has room.
        let limit = if target.one_shot
            && !found.referenced
            && capacity > target.block_max + 2 * WILDCOPY + found.length
        {
            target.at + target.block_max + WILDCOPY
        } else {
            target.capacity_end
        };
        let mut literals = LiteralRun {
            bytes: if found.in_block {
                &self.block
            } else {
                &self.literals
            },
            at: found.from,
            end: found.from + found.length,
        };
        let bounds = Bounds {
            limit,
            fast_end: limit.min(target.output.len().saturating_sub(WILDCOPY)),
            earlier: target.earlier,
            window: target.window,
        };
        let mut largest_offset = 0;
        if count > 0 {
            self.sequences_ready = true;
            let tables = Tables {
                literal_lengths: &self.literal_lengths,
                offsets: &self.offsets,
                match_lengths: &self.match_lengths,
            };
            let mut history = SequenceHistory {
                tables: &tables,
                repeats: &mut self.repeats,
                bounds: &bounds,
            };
            let progress = run_sequences(
                &self.block,
                (at, FRONT + size - at),
                count,
                &mut history,
                &literals,
                target.output,
                target.at,
            )?;
            target.at = progress.to;
            literals.at = progress.literal_at;
            largest_offset = progress.largest_offset;
        }
        target.at = rest_of_literals(target.output, target.at, limit, &literals)?;
        ensure(largest_offset <= target.window, BEYOND_WINDOW)
    }
}

// EXTERNAL(libzstd 1.5.7): ZSTD_decodeSequence refills here, so 64 bits always hold the fields between two refills.
const REFILL_BEFORE_LITERAL_LENGTH: u32 = 31;

struct Tables<'a> {
    literal_lengths: &'a SequenceTable<512>,
    offsets: &'a SequenceTable<256>,
    match_lengths: &'a SequenceTable<512>,
}

struct SequenceHistory<'a> {
    tables: &'a Tables<'a>,
    repeats: &'a mut [usize; 3],
    bounds: &'a Bounds,
}

struct LiteralRun<'a> {
    bytes: &'a Padded,
    at: usize,
    end: usize,
}

#[derive(Clone, Copy)]
struct Bounds {
    limit: usize,
    fast_end: usize,
    earlier: usize,
    window: usize,
}

struct Progress {
    to: usize,
    literal_at: usize,
    largest_offset: usize,
}

fn copy_overlapping(output: &mut [u8], at: usize, offset: usize, length: usize) -> Checked<()> {
    ensure(
        offset <= at && offset > 0 && at + length <= output.len(),
        DAMAGED_BLOCK,
    )?;
    let from = at - offset;
    let mut copied = 0;
    while copied < length {
        let run = (length - copied).min(offset + copied);
        output.copy_within(from..from + run, at + copied);
        copied += run;
    }
    Ok(())
}

#[cold]
#[inline(never)]
fn careful_sequence(
    output: &mut [u8],
    to: usize,
    bounds: &Bounds,
    literals: &LiteralRun,
    (literal_length, offset, match_length): (usize, usize, usize),
) -> Checked<usize> {
    ensure(literal_length <= literals.end - literals.at, DAMAGED_BLOCK)?;
    ensure(
        literal_length + match_length <= bounds.limit.saturating_sub(to),
        PAST_SIZE,
    )?;
    let from = literals.at & INDEX_MASK;
    let source = literals
        .bytes
        .get(from..from + literal_length)
        .ok_or(Corrupt(DAMAGED_BLOCK))?;
    output[to..to + literal_length].copy_from_slice(source);
    careful_match(output, to + literal_length, bounds, offset, match_length)
}

#[cold]
#[inline(never)]
fn careful_match(
    output: &mut [u8],
    mut to: usize,
    bounds: &Bounds,
    offset: usize,
    mut length: usize,
) -> Checked<usize> {
    ensure(length <= bounds.limit.saturating_sub(to), PAST_SIZE)?;
    let mut distance = offset;
    if offset > to {
        let before = offset - to;
        ensure(offset <= bounds.window, BEYOND_WINDOW)?;
        ensure(before <= bounds.earlier, DAMAGED_BLOCK)?;
        let from = bounds.earlier - before;
        let first = before.min(length);
        ensure(from + first <= output.len(), DAMAGED_BLOCK)?;
        output.copy_within(from..from + first, to);
        to += first;
        length -= first;
        distance = to;
    }
    if length > 0 {
        copy_overlapping(output, to, distance, length)?;
    }
    Ok(to + length)
}

#[cold]
#[inline(never)]
fn long_literals(
    output: &mut [u8],
    to: usize,
    literals: &Padded,
    from: usize,
    length: usize,
) -> Checked<()> {
    let (Some(target), Some(source)) = (
        output.get_mut(to + 32..to + length),
        literals.get(from + 32..from + length),
    ) else {
        return corrupt(DAMAGED_BLOCK);
    };
    target.copy_from_slice(source);
    Ok(())
}

fn rest_of_literals(
    output: &mut [u8],
    to: usize,
    limit: usize,
    literals: &LiteralRun,
) -> Checked<usize> {
    let rest = literals.end - literals.at;
    ensure(rest <= limit.saturating_sub(to), PAST_SIZE)?;
    let from = literals.at & INDEX_MASK;
    let source = literals
        .bytes
        .get(from..from + rest)
        .ok_or(Corrupt(DAMAGED_BLOCK))?;
    output[to..to + rest].copy_from_slice(source);
    Ok(to + rest)
}

// SPEC(RFC 8878 3.1.1.3.2.1.2 and 3.1.1.4): each sequence is an offset, a match length and a literal length, read backwards.
#[inline(never)]
fn run_sequences(
    block: &Padded,
    stream: (usize, usize),
    count: usize,
    history: &mut SequenceHistory,
    literals: &LiteralRun,
    output: &mut [u8],
    to: usize,
) -> Checked<Progress> {
    let (start, length) = stream;
    let tables = history.tables;
    let bounds = history.bounds;
    ensure(length > 0, DAMAGED_BLOCK)?;
    let last = byte(block, start + length - 1);
    ensure(last != 0, DAMAGED_BLOCK)?;
    let mut at = start + length - 8;
    let mut container;
    let mut consumed = 8 - high_bit(u32::from(last));

    macro_rules! refill {
        () => {
            at = at.wrapping_sub((consumed >> 3) as usize);
            consumed &= 7;
            container = le64(block, at);
        };
    }
    macro_rules! take {
        ($bits:expr) => {{
            let bits: u32 = $bits;
            let value = ((container << consumed) >> 1) >> (63 - bits);
            consumed += bits;
            value as usize
        }};
    }

    refill!();
    let mut literal_state = take!(tables.literal_lengths.log);
    let mut offset_state = take!(tables.offsets.log);
    let mut match_state = take!(tables.match_lengths.log);
    refill!();
    let [mut first, mut second, mut third] = *history.repeats;
    let mut to = to;
    let mut literal_at = literals.at;
    let mut largest_offset = 0usize;

    for left in (0..count).rev() {
        let literal = tables.literal_lengths.states[literal_state & 511];
        let matched = tables.match_lengths.states[match_state & 511];
        let offset_code = tables.offsets.states[offset_state & 255];
        let offset_bits = offset_code.extra();
        let offset;
        if offset_bits > 1 {
            offset = offset_code.base().wrapping_add(take!(offset_bits));
            third = second;
            second = first;
            first = offset;
        } else {
            let no_literals = literal.base() == 0;
            if offset_bits == 0 {
                if no_literals {
                    std::mem::swap(&mut first, &mut second);
                }
                offset = first;
            } else {
                let choice = offset_code.base() + usize::from(no_literals) + take!(1);
                let picked = match choice {
                    1 => second,
                    2 => third,
                    _ => first.wrapping_sub(1),
                };
                let picked = if picked == 0 { usize::MAX } else { picked };
                if choice != 1 {
                    third = second;
                }
                second = first;
                first = picked;
                offset = picked;
            }
        }
        let match_bits = matched.extra();
        let literal_bits = literal.extra();
        let match_length = matched.base() + take!(match_bits);
        if offset_bits + match_bits + literal_bits >= REFILL_BEFORE_LITERAL_LENGTH {
            refill!();
        }
        let literal_length = literal.base() + take!(literal_bits);
        if left != 0 {
            literal_state = literal.next() + take!(literal.bits());
            match_state = matched.next() + take!(matched.bits());
            offset_state = offset_code.next() + take!(offset_code.bits());
            refill!();
        }

        largest_offset = largest_offset.max(offset);
        let literal_stop = literal_at + literal_length;
        let end = to + literal_length + match_length;
        if literal_stop > literals.end || end > bounds.fast_end {
            let run = LiteralRun {
                bytes: literals.bytes,
                at: literal_at,
                end: literals.end,
            };
            to = careful_sequence(
                output,
                to,
                bounds,
                &run,
                (literal_length, offset, match_length),
            )?;
            literal_at = literal_stop;
            continue;
        }
        let from = literal_at & INDEX_MASK;
        let Some(head) = output.get_mut(to..to + 32) else {
            return corrupt(DAMAGED_BLOCK);
        };
        head.copy_from_slice(&literals.bytes[from..from + 32]);
        if literal_length > 32 {
            long_literals(output, to, literals.bytes, from, literal_length)?;
        }
        to += literal_length;
        literal_at = literal_stop;
        let mut from = to.wrapping_sub(offset);
        let mut distance = offset;
        if offset > to {
            let before = offset - to;
            if offset > bounds.window || before > bounds.earlier || before < match_length {
                to = careful_match(output, to, bounds, offset, match_length)?;
                continue;
            }
            from = bounds.earlier - before;
            distance = usize::MAX;
        }
        let rounded = (match_length + 31) & !31;
        if to + rounded > output.len() || from + rounded > output.len() {
            return corrupt(DAMAGED_BLOCK);
        }
        if distance >= 32 {
            let mut copied = 0;
            while copied < match_length {
                output.copy_within(from + copied..from + copied + 32, to + copied);
                copied += 32;
            }
        } else if distance >= 16 {
            let mut copied = 0;
            while copied < match_length {
                output.copy_within(from + copied..from + copied + 16, to + copied);
                copied += 16;
            }
        } else {
            let mut period = distance.max(1);
            while period < 16 {
                output.copy_within(from..from + 16, from + period);
                period *= 2;
            }
            let mut next = from + period;
            while next < end {
                output.copy_within(next - period..next - period + 16, next);
                next += 16;
            }
        }
        to = end;
    }
    *history.repeats = [first, second, third];
    let left = (at.wrapping_sub(start) as isize) * 8 + 64 - consumed as isize;
    ensure(left == 0, DAMAGED_BLOCK)?;
    Ok(Progress {
        to,
        literal_at,
        largest_offset,
    })
}

struct FrameHeader {
    window: u64,
    block_max: usize,
    content_size: Option<u64>,
    checksum: bool,
}

const NOT_A_FRAME: &str = "it does not start with a Zstandard frame";
const LEGACY_FRAME: &str = "it is a frame older than Zstandard 1.0, which Zipper does not read";
const LEGACY_MAGICS: std::ops::RangeInclusive<u32> = 0xFD2F_B522..=0xFD2F_B527;

fn not_a_frame(magic: u32) -> Corrupt {
    Corrupt(if LEGACY_MAGICS.contains(&magic) {
        LEGACY_FRAME
    } else {
        NOT_A_FRAME
    })
}
const DAMAGED_HEADER: &str = "the frame header is damaged";

fn frame_header_length(descriptor: u8) -> usize {
    let single = descriptor & 0x20 != 0;
    let dictionary = [0, 1, 2, 4][usize::from(descriptor & 3)];
    let content = match descriptor >> 6 {
        0 => usize::from(single),
        1 => 2,
        2 => 4,
        _ => 8,
    };
    usize::from(!single) + dictionary + content
}

// SPEC(RFC 8878 3.1.1.1): a descriptor byte, then the window, the dictionary ID and the content size it announces.
fn frame_header(descriptor: u8, fields: &[u8]) -> Checked<FrameHeader> {
    ensure(descriptor & 0x08 == 0, DAMAGED_HEADER)?;
    ensure(
        fields.len() == frame_header_length(descriptor),
        DAMAGED_HEADER,
    )?;
    let single = descriptor & 0x20 != 0;
    let mut at = 0;
    let mut window = 0u64;
    if !single {
        let log = u32::from(fields[0] >> 3) + 10;
        ensure(
            log <= WINDOW_LOG_MAX,
            "the frame's window is larger than Zstandard allows",
        )?;
        window = 1u64 << log;
        window += (window >> 3) * u64::from(fields[0] & 7);
        at = 1;
    }
    let dictionary = [0, 1, 2, 4][usize::from(descriptor & 3)];
    ensure(
        fields[at..at + dictionary].iter().all(|&byte| byte == 0),
        "the frame needs a dictionary",
    )?;
    at += dictionary;
    let rest = &fields[at..];
    let mut number = [0u8; 8];
    number[..rest.len()].copy_from_slice(rest);
    let number = u64::from_le_bytes(number);
    // EXTERNAL(libzstd 1.5.7): a content size of all ones is ZSTD_CONTENTSIZE_UNKNOWN, so it is never checked.
    let content_size = match descriptor >> 6 {
        0 => single.then_some(number),
        1 => Some(number + 256),
        _ => (number != u64::MAX).then_some(number),
    };
    if single {
        window = content_size.unwrap_or(u64::MAX);
    }
    Ok(FrameHeader {
        window,
        block_max: window.min(BLOCK_MAX as u64) as usize,
        content_size,
        checksum: descriptor & 0x04 != 0,
    })
}

struct BlockHeader {
    last: bool,
    kind: u8,
    size: usize,
}

fn block_header(bytes: [u8; 3]) -> BlockHeader {
    let header = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]);
    BlockHeader {
        last: header & 1 != 0,
        kind: ((header >> 1) & 3) as u8,
        size: (header >> 3) as usize,
    }
}

pub(crate) fn decode_frames_into(source: &[u8], output: &mut [u8]) -> Result<usize, Corrupt> {
    let mut decoder = LeasedDecoder(None);
    let mut input = source;
    let mut written = 0usize;
    const CUT: &str = "the data ends inside a frame";
    while input.len() >= 5 {
        let magic = u32::from_le_bytes(input[..4].try_into().unwrap());
        if magic & SKIPPABLE_MASK == SKIPPABLE_MAGIC {
            ensure(input.len() >= 8, CUT)?;
            let size = u32::from_le_bytes(input[4..8].try_into().unwrap()) as usize;
            ensure(size <= input.len() - 8, CUT)?;
            input = &input[8 + size..];
            continue;
        }
        if magic != MAGIC {
            return Err(not_a_frame(magic));
        }
        let descriptor = input[4];
        let fields = frame_header_length(descriptor);
        ensure(input.len() >= 5 + fields + 3, CUT)?;
        let header = frame_header(descriptor, &input[5..5 + fields])?;
        input = &input[5 + fields..];
        let mut fresh = true;
        let start = written;
        loop {
            ensure(input.len() >= 3, CUT)?;
            let block = block_header([input[0], input[1], input[2]]);
            input = &input[3..];
            let stored = if block.kind == 1 { 1 } else { block.size };
            ensure(block.kind != 3, DAMAGED_BLOCK)?;
            ensure(stored <= input.len(), CUT)?;
            let room = output.len() - written;
            match block.kind {
                0 => {
                    ensure(block.size <= room, PAST_SIZE)?;
                    output[written..written + block.size].copy_from_slice(&input[..block.size]);
                    written += block.size;
                }
                1 => {
                    ensure(block.size <= room, PAST_SIZE)?;
                    output[written..written + block.size].fill(input[0]);
                    written += block.size;
                }
                _ => {
                    ensure(block.size <= header.block_max, DAMAGED_BLOCK)?;
                    let decoder = decoder.held();
                    if std::mem::take(&mut fresh) {
                        decoder.start_frame();
                    }
                    decoder.block[FRONT..FRONT + block.size].copy_from_slice(&input[..block.size]);
                    let frame = &mut output[start..];
                    let mut target = Target {
                        at: written - start,
                        earlier: 0,
                        capacity_end: frame.len(),
                        block_max: header.block_max,
                        window: usize::MAX,
                        one_shot: true,
                        output: frame,
                    };
                    decoder.compressed_block(block.size, &mut target)?;
                    written = start + target.at;
                }
            }
            input = &input[stored..];
            if block.last {
                break;
            }
        }
        if let Some(size) = header.content_size {
            ensure((written - start) as u64 == size, SIZE_DIFFERS)?;
        }
        if header.checksum {
            ensure(input.len() >= 4, CUT)?;
            let mut hash = Xxh64::new();
            hash.update(&output[start..written]);
            ensure(
                input[..4] == (hash.digest() as u32).to_le_bytes(),
                "the frame's checksum does not match",
            )?;
            input = &input[4..];
        }
    }
    ensure(input.is_empty(), CUT)?;
    Ok(written)
}

enum Stage {
    Header,
    Blocks,
    Done,
    Failed,
}

struct Frame {
    header: FrameHeader,
    produced: u64,
    hash: Xxh64,
}

pub(crate) struct ZstdReader<R> {
    input: R,
    single_frame: bool,
    stage: Stage,
    decoder: LeasedDecoder,
    fresh_frame: bool,
    ring: Vec<u8>,
    lap: usize,
    at: usize,
    flushed: usize,
    earlier: usize,
    frame: Frame,
    finished_a_frame: bool,
}

fn cut_short() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "Zstandard data ends inside a frame",
    )
}

fn fill(input: &mut impl Read, bytes: &mut [u8]) -> io::Result<()> {
    input.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            cut_short()
        } else {
            error
        }
    })
}

impl<R: BufRead> ZstdReader<R> {
    pub(crate) fn new(input: R) -> Self {
        ZstdReader {
            input,
            single_frame: false,
            stage: Stage::Header,
            decoder: LeasedDecoder(None),
            fresh_frame: true,
            ring: Vec::new(),
            lap: 0,
            at: 0,
            flushed: 0,
            earlier: 0,
            frame: Frame {
                header: FrameHeader {
                    window: 0,
                    block_max: 0,
                    content_size: None,
                    checksum: false,
                },
                produced: 0,
                hash: Xxh64::new(),
            },
            finished_a_frame: false,
        }
    }

    pub(crate) fn single_frame(mut self) -> Self {
        self.single_frame = true;
        self
    }

    fn skip(&mut self, mut bytes: u64) -> io::Result<()> {
        while bytes > 0 {
            let held = match self.input.fill_buf() {
                Ok(held) => held.len(),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if held == 0 {
                return Err(cut_short());
            }
            let step = (held as u64).min(bytes);
            self.input.consume(step as usize);
            bytes -= step;
        }
        Ok(())
    }

    fn finish_frame(&mut self) {
        self.finished_a_frame = true;
        self.stage = if self.single_frame {
            Stage::Done
        } else {
            Stage::Header
        };
    }

    fn start_frame(&mut self) -> io::Result<()> {
        if self.input.fill_buf()?.is_empty() {
            if self.finished_a_frame {
                self.stage = Stage::Done;
                return Ok(());
            }
            return Err(cut_short());
        }
        let mut magic = [0u8; 4];
        fill(&mut self.input, &mut magic)?;
        let magic = u32::from_le_bytes(magic);
        if magic & SKIPPABLE_MASK == SKIPPABLE_MAGIC {
            let mut size = [0u8; 4];
            fill(&mut self.input, &mut size)?;
            self.skip(u64::from(u32::from_le_bytes(size)))?;
            self.finish_frame();
            return Ok(());
        }
        if magic != MAGIC {
            return Err(not_a_frame(magic).into());
        }
        let mut descriptor = [0u8; 1];
        fill(&mut self.input, &mut descriptor)?;
        let mut fields = [0u8; 13];
        let fields = &mut fields[..frame_header_length(descriptor[0])];
        fill(&mut self.input, fields)?;
        let header = frame_header(descriptor[0], fields)?;
        if header.window > WINDOW_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!(
                    "Zstandard frame declares a {}-byte window, more than the {} MiB Zipper accepts",
                    header.window,
                    WINDOW_LIMIT >> 20
                ),
            ));
        }
        let lap = header.window as usize + header.block_max;
        self.lap = match header.content_size {
            Some(size) if size <= lap as u64 => size as usize + RING_SLACK,
            _ => lap + 2 * RING_SLACK,
        };
        if self.ring.capacity() < self.lap {
            self.ring = Vec::new();
            self.ring = Vec::with_capacity(self.lap);
        }
        self.at = 0;
        self.flushed = 0;
        self.earlier = 0;
        self.fresh_frame = true;
        self.frame = Frame {
            header,
            produced: 0,
            hash: Xxh64::new(),
        };
        self.stage = Stage::Blocks;
        Ok(())
    }

    fn make_room(&mut self, needed: usize) {
        let end = self.at + needed + RING_SLACK;
        if end > self.ring.len() && self.ring.len() < self.lap {
            self.ring.resize(self.lap.min(end), 0);
        }
        if end > self.ring.len() {
            self.earlier = self.at;
            self.at = 0;
            self.flushed = 0;
        }
    }

    fn next_block(&mut self) -> io::Result<()> {
        let mut bytes = [0u8; 3];
        fill(&mut self.input, &mut bytes)?;
        let block = block_header(bytes);
        let block_max = self.frame.header.block_max;
        let left = match self.frame.header.content_size {
            Some(size) => size
                .saturating_sub(self.frame.produced)
                .min(block_max as u64) as usize,
            None => block_max,
        };
        let stored = if block.kind == 1 { 1 } else { block.size };
        ensure(block.kind != 3 && stored <= block_max, DAMAGED_BLOCK)?;
        let copied = block.kind < 2;
        ensure(!copied || block.size <= block_max, DAMAGED_BLOCK)?;
        ensure(!copied || block.size <= left, PAST_SIZE)?;
        self.make_room(if copied { block.size } else { left });
        let from = self.at;
        match block.kind {
            0 | 1 => {
                if block.kind == 0 {
                    fill(&mut self.input, &mut self.ring[from..from + block.size])?;
                } else {
                    let mut repeated = [0u8; 1];
                    fill(&mut self.input, &mut repeated)?;
                    self.ring[from..from + block.size].fill(repeated[0]);
                }
                self.at = from + block.size;
            }
            _ if block.size == 0 => {}
            _ => {
                let decoder = self.decoder.held();
                if std::mem::take(&mut self.fresh_frame) {
                    decoder.start_frame();
                }
                fill(
                    &mut self.input,
                    &mut decoder.block[FRONT..FRONT + block.size],
                )?;
                let mut target = Target {
                    at: from,
                    earlier: self.earlier,
                    capacity_end: from + left,
                    block_max,
                    window: usize::try_from(self.frame.header.window).unwrap_or(usize::MAX),
                    one_shot: false,
                    output: &mut self.ring,
                };
                decoder.compressed_block(block.size, &mut target)?;
                self.at = target.at;
            }
        }
        let decoded = &self.ring[from..self.at];
        self.frame.produced += decoded.len() as u64;
        if self.frame.header.checksum {
            self.frame.hash.update(decoded);
        }
        if !block.last {
            return Ok(());
        }
        if let Some(size) = self.frame.header.content_size {
            ensure(self.frame.produced == size, SIZE_DIFFERS)?;
        }
        if self.frame.header.checksum {
            let mut stored = [0u8; 4];
            fill(&mut self.input, &mut stored)?;
            ensure(
                stored == (self.frame.hash.digest() as u32).to_le_bytes(),
                "the frame's checksum does not match",
            )?;
        }
        self.finish_frame();
        Ok(())
    }
}

impl<R: BufRead> Read for ZstdReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            if self.flushed < self.at {
                let count = buffer.len().min(self.at - self.flushed);
                buffer[..count].copy_from_slice(&self.ring[self.flushed..self.flushed + count]);
                self.flushed += count;
                return Ok(count);
            }
            let step = match self.stage {
                Stage::Done => return Ok(0),
                Stage::Failed => {
                    return Err(io::Error::other("Zstandard decoding already failed"));
                }
                Stage::Header => self.start_frame(),
                Stage::Blocks => self.next_block(),
            };
            if let Err(error) = step {
                if error.kind() != io::ErrorKind::Interrupted {
                    self.stage = Stage::Failed;
                    self.at = self.flushed;
                }
                return Err(error);
            }
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const PROVED_WEIGHTS: usize = 8;

    #[kani::proof]
    #[kani::unwind(15)]
    fn weights_that_pass_form_a_complete_tree_with_its_deepest_leaves_in_pairs() {
        let mut listed_weights = [0u8; SYMBOLS];
        let listed: usize = kani::any();
        kani::assume(listed <= PROVED_WEIGHTS);
        for weight in listed_weights.iter_mut().take(PROVED_WEIGHTS) {
            *weight = kani::any();
        }
        let Ok(tree) = weights_from_list(listed_weights, listed, 0) else {
            return;
        };
        assert!((1..=HUFFMAN_LOG_MAX).contains(&tree.log) && tree.symbols == listed + 1);
        let mut total = 0u32;
        let mut counted = [0u32; HUFFMAN_LOG_MAX as usize + 1];
        for symbol in 0..=listed {
            let weight = tree.weights[symbol];
            assert!(u32::from(weight) <= tree.log);
            total += (1u32 << weight) >> 1;
            counted[usize::from(weight)] += 1;
        }
        assert!(total == 1 << tree.log && tree.weights[listed] != 0);
        for (counted, rank) in counted.iter().zip(&tree.ranks) {
            assert!(counted == rank);
        }
        assert!(tree.ranks[1] >= 2 && tree.ranks[1] % 2 == 0);
    }

    #[kani::proof]
    fn refills_keep_the_reader_inside_its_stream_and_its_count_of_bits() {
        let before = BitD {
            container: 0,
            consumed: kani::any(),
            at: kani::any(),
            start: kani::any(),
        };
        kani::assume(before.start <= PADDED && before.at >= before.start);
        kani::assume(before.at - before.start <= PADDED && before.consumed <= 1 << 20);
        let left = |bit: &BitD| (bit.at - bit.start) as i64 * 8 + 64 - i64::from(bit.consumed);
        let mut after = before;
        let (status, moved) = after.step_back();
        assert!(after.at >= after.start && after.at <= before.at);
        assert!(moved || after == before);
        if status == Refill::Overflow {
            assert!(before.consumed > 64 && after == before);
        } else {
            assert!(left(&after) == left(&before) && after.consumed <= 64);
            assert!(after.at == after.start || after.consumed < 8);
            assert!((status == Refill::Completed) == (after.ended() && !moved));
        }
    }

    #[kani::proof]
    #[kani::unwind(15)]
    fn frame_headers_never_panic_and_bound_their_window_and_blocks() {
        let descriptor: u8 = kani::any();
        let bytes: [u8; 14] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        assert!(frame_header_length(descriptor) <= 13);
        if let Ok(header) = frame_header(descriptor, &bytes[..length]) {
            assert!(length == frame_header_length(descriptor) && descriptor & 0x08 == 0);
            assert!(header.block_max <= BLOCK_MAX && header.block_max as u64 <= header.window);
            assert!(descriptor & 0x20 != 0 || header.window >= 1 << 10);
            assert!(descriptor & 0x20 != 0 || header.window <= 15 << (WINDOW_LOG_MAX - 3));
            assert!(header.checksum == (descriptor & 0x04 != 0));
            assert!(header.content_size != Some(u64::MAX) || descriptor >> 6 < 2);
        }
        let block = block_header([bytes[0], bytes[1], bytes[2]]);
        assert!(block.kind < 4 && block.size < 1 << 21);
    }
}

#[cfg(test)]
mod tests;
