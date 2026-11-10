use std::sync::OnceLock;

pub(crate) const HISTORY: usize = 32 * 1024;
pub(crate) const SMALLEST_WINDOW: usize = 2 * HISTORY;

const LONGEST_MATCH: usize = 258;
const LITERALS_BEFORE_A_REFILL: usize = 3;
const COPY_OVERRUN: usize = 32;
const OUTPUT_MARGIN: usize = LITERALS_BEFORE_A_REFILL + LONGEST_MATCH + COPY_OVERRUN;
const REFILLS_IN_ONE_PASS: usize = 3;
const FAST_INPUT: usize = REFILLS_IN_ONE_PASS * 7 + 8;

const LONGEST_CODE: usize = 15;
const LITLEN_BITS: u32 = 11;
const DISTANCE_BITS: u32 = 8;
const PRECODE_BITS: u32 = 7;
// LINK: https://github.com/madler/zlib/blob/v1.3.1/examples/enough.c
const LITLEN_ENTRIES: usize = 2342;
const DISTANCE_ENTRIES: usize = 402;
const PRECODE_ENTRIES: usize = 1 << PRECODE_BITS;

const LITLEN_SYMBOLS: usize = 288;
const DISTANCE_SYMBOLS: usize = 32;
const PRECODE_SYMBOLS: usize = 19;
const END_OF_BLOCK_SYMBOL: usize = 256;
const MOST_LITLEN_CODES: usize = 286;
const MOST_DISTANCE_CODES: usize = 30;

const MOST_MAIN_DISTANCE_BITS: u32 = DISTANCE_BITS + 13;
const UNCOUNTED_BITS_AFTER_A_REFILL: u32 = 1;

const LITERAL: u32 = 1 << 31;
const EXCEPTIONAL: u32 = 1 << 15;
const SUBTABLE: u32 = 1 << 14;
const END_OF_BLOCK: u32 = 1 << 13;
const VALUE_SHIFT: u32 = 16;
const CODE_BITS_SHIFT: u32 = 8;
const CODE_BITS_MASK: u32 = 0xf;
const STEP_BITS_MASK: u32 = 0xff;
const UNUSED_CODE: u32 = EXCEPTIONAL | 1 << CODE_BITS_SHIFT | 1;

// SPEC(RFC 1951 3.2.5): length codes 257 to 285 and distance codes 0 to 29.
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
// SPEC(RFC 1951 3.2.7): the order in which code length code lengths are stored.
const PRECODE_ORDER: [u8; PRECODE_SYMBOLS] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Invalid(pub(crate) &'static str);

impl std::fmt::Display for Invalid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for Invalid {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Alphabet {
    Precode,
    LitLen,
    Distance,
}

const fn litlen_symbols() -> [u32; LITLEN_SYMBOLS] {
    let mut symbols = [EXCEPTIONAL; LITLEN_SYMBOLS];
    let mut symbol = 0;
    while symbol < END_OF_BLOCK_SYMBOL {
        symbols[symbol] = LITERAL | (symbol as u32) << VALUE_SHIFT;
        symbol += 1;
    }
    symbols[END_OF_BLOCK_SYMBOL] = EXCEPTIONAL | END_OF_BLOCK;
    let mut index = 0;
    while index < LENGTH_BASE.len() {
        symbols[END_OF_BLOCK_SYMBOL + 1 + index] =
            (LENGTH_BASE[index] as u32) << VALUE_SHIFT | LENGTH_EXTRA[index] as u32;
        index += 1;
    }
    symbols
}

const fn distance_symbols() -> [u32; DISTANCE_SYMBOLS] {
    let mut symbols = [EXCEPTIONAL; DISTANCE_SYMBOLS];
    let mut index = 0;
    while index < DISTANCE_BASE.len() {
        symbols[index] =
            (DISTANCE_BASE[index] as u32) << VALUE_SHIFT | DISTANCE_EXTRA[index] as u32;
        index += 1;
    }
    symbols
}

const fn precode_symbols() -> [u32; PRECODE_SYMBOLS] {
    let mut symbols = [0; PRECODE_SYMBOLS];
    let mut symbol = 0;
    while symbol < PRECODE_SYMBOLS {
        symbols[symbol] = (symbol as u32) << VALUE_SHIFT;
        symbol += 1;
    }
    symbols
}

static LITLEN_SYMBOL_ENTRIES: [u32; LITLEN_SYMBOLS] = litlen_symbols();
static DISTANCE_SYMBOL_ENTRIES: [u32; DISTANCE_SYMBOLS] = distance_symbols();
static PRECODE_SYMBOL_ENTRIES: [u32; PRECODE_SYMBOLS] = precode_symbols();

impl Alphabet {
    fn symbols(self) -> &'static [u32] {
        match self {
            Alphabet::Precode => &PRECODE_SYMBOL_ENTRIES,
            Alphabet::LitLen => &LITLEN_SYMBOL_ENTRIES,
            Alphabet::Distance => &DISTANCE_SYMBOL_ENTRIES,
        }
    }
}

fn entry(symbols: &[u32], symbol: u16, code_bits: usize) -> u32 {
    let code_bits = code_bits as u32;
    symbols[usize::from(symbol)] + (code_bits << CODE_BITS_SHIFT) + code_bits
}

fn low_bits(count: u32) -> u64 {
    (1u64 << count) - 1
}

fn next_reversed_code(code: usize, length: usize) -> Option<usize> {
    let zeros = !code & ((1 << length) - 1);
    let highest_zero = 1 << zeros.checked_ilog2()?;
    Some(code & (highest_zero - 1) | highest_zero)
}

fn double_up_to(table: &mut [u32], mut filled: usize, size: usize) {
    while filled < size {
        table.copy_within(..filled, filled);
        filled *= 2;
    }
}

const SYMBOL_WORDS: usize = LITLEN_SYMBOLS.div_ceil(u64::BITS as usize);

fn each_coded_symbol(coded: &[u64; SYMBOL_WORDS], mut visit: impl FnMut(usize)) {
    for (word, &symbols) in coded.iter().enumerate() {
        let mut rest = symbols;
        while rest != 0 {
            visit(word * u64::BITS as usize + rest.trailing_zeros() as usize);
            rest &= rest - 1;
        }
    }
}

fn build_table(
    table: &mut [u32],
    lengths: &[u8],
    most_table_bits: u32,
    alphabet: Alphabet,
) -> Result<u32, Invalid> {
    const UNUSABLE: Invalid = Invalid("Deflate code lengths do not fit their table");
    const ONE_BIT: u32 = 1;
    let symbols = alphabet.symbols();
    if lengths.len() > symbols.len() || table.len() < 1 << most_table_bits.max(ONE_BIT) {
        return Err(UNUSABLE);
    }
    let mut coded = [0u64; SYMBOL_WORDS];
    let mut every_length = 0;
    for (word, lengths) in coded.iter_mut().zip(lengths.chunks(u64::BITS as usize)) {
        for (symbol, &length) in lengths.iter().enumerate() {
            *word |= u64::from(length != 0) << symbol;
            every_length |= length;
        }
    }
    if usize::from(every_length) > LONGEST_CODE {
        return Err(UNUSABLE);
    }
    let length_of = |symbol: usize| usize::from(lengths[symbol]) & LONGEST_CODE;
    let mut counts = [0u16; LONGEST_CODE + 1];
    each_coded_symbol(&coded, |symbol| counts[length_of(symbol)] += 1);
    let Some(longest) = (1..=LONGEST_CODE).rev().find(|&length| counts[length] != 0) else {
        // EXTERNAL(zlib): a block with no distance code is valid until it uses a distance.
        if alphabet != Alphabet::Distance {
            return Err(Invalid("Deflate block has no code lengths"));
        }
        table[..2].fill(UNUSED_CODE);
        return Ok(ONE_BIT);
    };
    let mut unused = 1i32;
    for &count in &counts[1..] {
        unused = (unused << 1) - i32::from(count);
        if unused < 0 {
            return Err(Invalid("Deflate code lengths are over-subscribed"));
        }
    }

    let mut offsets = [0u16; LONGEST_CODE + 2];
    for length in 1..=LONGEST_CODE {
        offsets[length + 1] = offsets[length] + counts[length];
    }
    let mut sorted = [0u16; LITLEN_SYMBOLS];
    each_coded_symbol(&coded, |symbol| {
        let slot = &mut offsets[length_of(symbol)];
        sorted[usize::from(*slot)] = symbol as u16;
        *slot += 1;
    });

    if unused > 0 {
        // EXTERNAL(zlib): an incomplete code is valid only as one 1-bit literal, length or distance code.
        if longest != 1 || alphabet == Alphabet::Precode {
            return Err(Invalid("Deflate code lengths are incomplete"));
        }
        table[0] = entry(symbols, sorted[0], 1);
        table[1] = UNUSED_CODE;
        return Ok(ONE_BIT);
    }

    let table_bits = most_table_bits.min(longest as u32);
    let main = 1usize << table_bits;
    let table_length = table_bits as usize;
    let mut sorted = sorted.iter().copied();
    let mut next_symbol = || sorted.next().ok_or(UNUSABLE);
    let mut code = 0;
    let mut length = 1;
    while counts[length] == 0 {
        length += 1;
    }
    let mut filled = 1 << length.min(table_length);
    while length <= table_length {
        for _ in 0..counts[length] {
            table[code] = entry(symbols, next_symbol()?, length);
            let Some(next) = next_reversed_code(code, length) else {
                double_up_to(table, filled, main);
                return Ok(table_bits);
            };
            code = next;
        }
        loop {
            length += 1;
            if length <= table_length {
                double_up_to(table, filled, 2 * filled);
                filled *= 2;
            }
            if *counts.get(length).ok_or(UNUSABLE)? != 0 {
                break;
            }
        }
    }

    let mut prefix = usize::MAX;
    let (mut subtable, mut end) = (0, main);
    loop {
        for placed in 0..counts[length] {
            if code & (main - 1) != prefix {
                prefix = code & (main - 1);
                subtable = end;
                let mut subtable_length = length - table_length;
                let mut codes = usize::from(counts[length] - placed);
                while codes < 1 << subtable_length {
                    subtable_length += 1;
                    let longer = counts.get(table_length + subtable_length).ok_or(UNUSABLE)?;
                    codes = (codes << 1) + usize::from(*longer);
                }
                end = subtable + (1 << subtable_length);
                if end > table.len() {
                    return Err(UNUSABLE);
                }
                table[prefix] = (subtable as u32) << VALUE_SHIFT
                    | EXCEPTIONAL
                    | SUBTABLE
                    | (subtable_length as u32) << CODE_BITS_SHIFT
                    | table_bits;
            }
            let entry = entry(symbols, next_symbol()?, length - table_length);
            let first = subtable + (code >> table_length);
            for slot in table[first..end]
                .iter_mut()
                .step_by(1 << (length - table_length))
            {
                *slot = entry;
            }
            let Some(next) = next_reversed_code(code, length) else {
                return Ok(table_bits);
            };
            code = next;
        }
        length += 1;
        while *counts.get(length).ok_or(UNUSABLE)? == 0 {
            length += 1;
        }
    }
}

struct Tables {
    litlen: [u32; LITLEN_ENTRIES],
    distance: [u32; DISTANCE_ENTRIES],
    litlen_bits: u32,
    distance_bits: u32,
}

impl Tables {
    const fn empty() -> Self {
        Self {
            litlen: [0; LITLEN_ENTRIES],
            distance: [0; DISTANCE_ENTRIES],
            litlen_bits: 0,
            distance_bits: 0,
        }
    }

    fn build(&mut self, litlen: &[u8], distance: &[u8]) -> Result<(), Invalid> {
        self.litlen_bits = build_table(&mut self.litlen, litlen, LITLEN_BITS, Alphabet::LitLen)?;
        self.distance_bits = build_table(
            &mut self.distance,
            distance,
            DISTANCE_BITS,
            Alphabet::Distance,
        )?;
        Ok(())
    }
}

fn fixed_tables() -> &'static Tables {
    static FIXED: OnceLock<Box<Tables>> = OnceLock::new();
    FIXED.get_or_init(|| {
        // SPEC(RFC 1951 3.2.6): the fixed code's lengths.
        let mut litlen = [8u8; LITLEN_SYMBOLS];
        litlen[144..256].fill(9);
        litlen[256..280].fill(7);
        let mut tables = Box::new(Tables::empty());
        tables
            .build(&litlen, &[5; DISTANCE_SYMBOLS])
            .expect("the fixed Deflate code is complete");
        tables
    })
}

#[derive(Clone, Copy, Default)]
struct Bits {
    buffer: u64,
    count: u32,
}

const MOST_BITS_IN_ONE_SYMBOL: u32 = 48;

impl Bits {
    fn pull(&mut self, input: &[u8], position: &mut usize) -> bool {
        let Some(&byte) = input.get(*position) else {
            return false;
        };
        debug_assert!(self.count < MOST_BITS_IN_ONE_SYMBOL);
        self.buffer |= u64::from(byte) << self.count;
        self.count += 8;
        *position += 1;
        true
    }

    fn need(&mut self, count: u32, input: &[u8], position: &mut usize) -> bool {
        while self.count < count {
            if !self.pull(input, position) {
                return false;
            }
        }
        true
    }

    fn skip(&mut self, count: u32) {
        self.buffer >>= count;
        self.count -= count;
    }

    fn take(&mut self, count: u32) -> u32 {
        let value = (self.buffer & low_bits(count)) as u32;
        self.skip(count);
        value
    }

    fn give_back_whole_bytes(&mut self, position: &mut usize) {
        let bytes = (self.count as usize >> 3).min(*position);
        *position -= bytes;
        self.count -= 8 * bytes as u32;
        self.buffer &= low_bits(self.count);
    }
}

fn extra_bits(before: u64, entry: u32) -> u32 {
    ((before & low_bits(entry & STEP_BITS_MASK)) >> ((entry >> CODE_BITS_SHIFT) & CODE_BITS_MASK))
        as u32
}

fn lookup(table: &[u32], table_bits: u32, bits: Bits) -> Option<(u32, u32)> {
    let mut entry = table[(bits.buffer & low_bits(table_bits)) as usize];
    let mut skipped = 0;
    if entry & SUBTABLE != 0 {
        skipped = table_bits;
        let subtable_bits = (entry >> CODE_BITS_SHIFT) & CODE_BITS_MASK;
        let index = (bits.buffer >> table_bits) & low_bits(subtable_bits);
        entry = table[(entry >> VALUE_SHIFT) as usize + index as usize];
    }
    (skipped + (entry & STEP_BITS_MASK) <= bits.count).then_some((entry, skipped))
}

enum Symbol {
    Literal(u8),
    EndOfBlock,
    Match { length: usize, distance: usize },
}

const INVALID_LITLEN: Invalid = Invalid("invalid Deflate literal or length code");
const INVALID_DISTANCE: Invalid = Invalid("invalid Deflate distance code");
const TOO_FAR_BACK: Invalid = Invalid("Deflate distance reaches before the start of the data");

fn symbol(tables: &Tables, bits: Bits) -> Result<Option<(Symbol, u32)>, Invalid> {
    let Some((entry, skipped)) = lookup(&tables.litlen, tables.litlen_bits, bits) else {
        return Ok(None);
    };
    let mut used = skipped + (entry & STEP_BITS_MASK);
    if entry & LITERAL != 0 {
        return Ok(Some((Symbol::Literal((entry >> VALUE_SHIFT) as u8), used)));
    }
    if entry & EXCEPTIONAL != 0 {
        return match entry & END_OF_BLOCK != 0 {
            true => Ok(Some((Symbol::EndOfBlock, used))),
            false => Err(INVALID_LITLEN),
        };
    }
    let length = (entry >> VALUE_SHIFT) + extra_bits(bits.buffer >> skipped, entry);
    let rest = Bits {
        buffer: bits.buffer >> used,
        count: bits.count - used,
    };
    let Some((entry, skipped)) = lookup(&tables.distance, tables.distance_bits, rest) else {
        return Ok(None);
    };
    if entry & EXCEPTIONAL != 0 {
        return Err(INVALID_DISTANCE);
    }
    let distance = (entry >> VALUE_SHIFT) + extra_bits(rest.buffer >> skipped, entry);
    used += skipped + (entry & STEP_BITS_MASK);
    Ok(Some((
        Symbol::Match {
            length: length as usize,
            distance: distance as usize,
        },
        used,
    )))
}

const WIDE_ENOUGH_TO_COPY_FROM: usize = 64;

#[inline(always)]
fn copy_match(window: &mut [u8], at: usize, distance: usize, length: usize) {
    let from = at - distance;
    if distance >= 16 {
        window.copy_within(from..from + 16, at);
        window.copy_within(from + 16..from + 32, at + 16);
        if length > 32 {
            let back = if distance <= 32 {
                2 * distance
            } else {
                distance
            };
            let mut copied = 32;
            while copied < length {
                let source = at + copied - back;
                window.copy_within(source..source + 16, at + copied);
                copied += 16;
            }
        }
    } else if distance == 1 {
        let byte = window[from];
        window[at..at + length].fill(byte);
    } else {
        copy_short_period(window, at, distance, length);
    }
}

#[inline(never)]
fn copy_short_period(window: &mut [u8], at: usize, distance: usize, length: usize) {
    let mut back = distance;
    let mut copied = 0;
    while copied < length {
        let source = at + copied - back;
        if back < 16 {
            window.copy_within(source..source + 8, at + copied);
            copied += back.min(8);
        } else {
            window.copy_within(source..source + 16, at + copied);
            copied += 16;
        }
        if back < WIDE_ENOUGH_TO_COPY_FROM && copied + distance >= 2 * back {
            back *= 2;
        }
    }
}

struct Cursor {
    bits: Bits,
    position: usize,
    filled: usize,
}

enum Pause {
    EndOfBlock,
    Margin,
}

#[inline(never)]
fn fast(
    tables: &Tables,
    input: &[u8],
    window: &mut [u8],
    cursor: Cursor,
) -> (Cursor, Result<Pause, Invalid>) {
    let Cursor {
        bits: Bits {
            buffer: mut bits,
            mut count,
        },
        mut position,
        mut filled,
    } = cursor;
    let litlen_mask = low_bits(tables.litlen_bits) & low_bits(LITLEN_BITS);
    let distance_mask = low_bits(tables.distance_bits) & low_bits(DISTANCE_BITS);
    let mut valid;
    macro_rules! refill {
        () => {{
            let word: [u8; 8] = input[position..position + 8]
                .try_into()
                .expect("the range is 8 bytes long");
            bits |= u64::from_le_bytes(word) << count;
            position += ((!count >> 3) & 7) as usize;
            count |= 56;
            valid = u64::BITS;
        }};
    }
    macro_rules! consume {
        ($entry:expr) => {{
            let before = bits;
            debug_assert!($entry & STEP_BITS_MASK <= count);
            bits >>= $entry & STEP_BITS_MASK;
            count -= $entry & STEP_BITS_MASK;
            valid -= $entry & STEP_BITS_MASK;
            before
        }};
    }
    macro_rules! litlen {
        () => {{
            debug_assert!(valid >= LITLEN_BITS);
            tables.litlen[(bits & litlen_mask) as usize]
        }};
    }
    macro_rules! margins {
        () => {
            position <= input.len()
                && input.len() - position >= FAST_INPUT
                && filled <= window.len()
                && window.len() - filled >= OUTPUT_MARGIN
        };
    }
    if !margins!() {
        let cursor = Cursor {
            bits: Bits {
                buffer: bits,
                count,
            },
            position,
            filled,
        };
        return (cursor, Ok(Pause::Margin));
    }
    refill!();
    let mut entry = litlen!();
    let exit = 'decode: loop {
        let mut before = consume!(entry);
        'symbol: {
            if entry & LITERAL != 0 {
                window[filled] = (entry >> VALUE_SHIFT) as u8;
                filled += 1;
                entry = litlen!();
                before = consume!(entry);
                if entry & LITERAL != 0 {
                    window[filled] = (entry >> VALUE_SHIFT) as u8;
                    filled += 1;
                    entry = litlen!();
                    before = consume!(entry);
                    if entry & LITERAL != 0 {
                        window[filled] = (entry >> VALUE_SHIFT) as u8;
                        filled += 1;
                        break 'symbol;
                    }
                }
            }
            if entry & EXCEPTIONAL != 0 {
                if entry & SUBTABLE == 0 {
                    break 'decode match entry & END_OF_BLOCK != 0 {
                        true => Ok(Pause::EndOfBlock),
                        false => Err(INVALID_LITLEN),
                    };
                }
                let index = bits & low_bits((entry >> CODE_BITS_SHIFT) & CODE_BITS_MASK);
                entry = tables.litlen[(entry >> VALUE_SHIFT) as usize + index as usize];
                before = consume!(entry);
                if entry & LITERAL != 0 {
                    window[filled] = (entry >> VALUE_SHIFT) as u8;
                    filled += 1;
                    break 'symbol;
                }
                if entry & EXCEPTIONAL != 0 {
                    break 'decode match entry & END_OF_BLOCK != 0 {
                        true => Ok(Pause::EndOfBlock),
                        false => Err(INVALID_LITLEN),
                    };
                }
            }
            let length = ((entry >> VALUE_SHIFT) + extra_bits(before, entry)) as usize;

            debug_assert!(valid >= DISTANCE_BITS);
            let mut entry = tables.distance[(bits & distance_mask) as usize];
            if entry & EXCEPTIONAL != 0 {
                if entry & SUBTABLE == 0 {
                    break 'decode Err(INVALID_DISTANCE);
                }
                refill!();
                consume!(entry);
                let index = bits & low_bits((entry >> CODE_BITS_SHIFT) & CODE_BITS_MASK);
                entry = tables.distance[(entry >> VALUE_SHIFT) as usize + index as usize];
                if entry & EXCEPTIONAL != 0 {
                    break 'decode Err(INVALID_DISTANCE);
                }
            } else if count < MOST_MAIN_DISTANCE_BITS + LITLEN_BITS - UNCOUNTED_BITS_AFTER_A_REFILL
            {
                refill!();
            }
            let before = consume!(entry);
            let distance = ((entry >> VALUE_SHIFT) + extra_bits(before, entry)) as usize;
            if distance > filled {
                break 'decode Err(TOO_FAR_BACK);
            }
            copy_match(window, filled, distance, length);
            filled += length;
        }
        entry = litlen!();
        if !margins!() {
            break 'decode Ok(Pause::Margin);
        }
        refill!();
    };
    let cursor = Cursor {
        bits: Bits {
            buffer: bits,
            count,
        },
        position,
        filled,
    };
    (cursor, exit)
}

#[derive(Clone, Copy)]
enum State {
    BlockHeader,
    StoredLength,
    Stored { remaining: usize },
    CodeCounts,
    PrecodeLengths,
    CodeLengths,
    Symbols,
    Finished,
    Failed(Invalid),
}

pub(crate) struct Step<'a> {
    pub(crate) consumed: usize,
    pub(crate) output: &'a [u8],
    pub(crate) finished: bool,
}

impl Step<'_> {
    pub(crate) fn needs_input(&self) -> bool {
        !self.finished && self.consumed == 0 && self.output.is_empty()
    }
}

struct Dynamic {
    litlen_codes: usize,
    distance_codes: usize,
    precode_codes: usize,
    lengths_read: usize,
    precode_lengths: [u8; PRECODE_SYMBOLS],
    lengths: [u8; LITLEN_SYMBOLS + DISTANCE_SYMBOLS],
    precode: [u32; PRECODE_ENTRIES],
    precode_bits: u32,
    tables: Tables,
}

impl Dynamic {
    fn boxed() -> Box<Self> {
        const EMPTY: Dynamic = Dynamic {
            litlen_codes: 0,
            distance_codes: 0,
            precode_codes: 0,
            lengths_read: 0,
            precode_lengths: [0; PRECODE_SYMBOLS],
            lengths: [0; LITLEN_SYMBOLS + DISTANCE_SYMBOLS],
            precode: [0; PRECODE_ENTRIES],
            precode_bits: 0,
            tables: Tables::empty(),
        };
        Box::new(EMPTY)
    }

    fn code_counts(&mut self, bits: &mut Bits) -> Result<(), Invalid> {
        self.litlen_codes = bits.take(5) as usize + 257;
        self.distance_codes = bits.take(5) as usize + 1;
        self.precode_codes = bits.take(4) as usize + 4;
        // EXTERNAL(zlib): more than 286 length or 30 distance codes is refused.
        if self.litlen_codes > MOST_LITLEN_CODES || self.distance_codes > MOST_DISTANCE_CODES {
            return Err(Invalid("too many Deflate length or distance codes"));
        }
        self.precode_lengths = [0; PRECODE_SYMBOLS];
        self.lengths_read = 0;
        Ok(())
    }

    fn precode(
        &mut self,
        bits: &mut Bits,
        input: &[u8],
        position: &mut usize,
    ) -> Result<bool, Invalid> {
        while self.lengths_read < self.precode_codes {
            if !bits.need(3, input, position) {
                return Ok(false);
            }
            let symbol = PRECODE_ORDER[self.lengths_read];
            self.precode_lengths[usize::from(symbol)] = bits.take(3) as u8;
            self.lengths_read += 1;
        }
        self.precode_bits = build_table(
            &mut self.precode,
            &self.precode_lengths,
            PRECODE_BITS,
            Alphabet::Precode,
        )?;
        self.lengths_read = 0;
        Ok(true)
    }

    fn codes(
        &mut self,
        bits: &mut Bits,
        input: &[u8],
        position: &mut usize,
    ) -> Result<bool, Invalid> {
        const BAD_REPEAT: Invalid = Invalid("invalid Deflate code length repeat");
        let total = self.litlen_codes + self.distance_codes;
        while self.lengths_read < total {
            // SPEC(RFC 1951 3.2.7): 16 repeats the last length, 17 and 18 repeat zero.
            let (symbol, code_bits, repeat_bits, repeat_base) = loop {
                let entry = self.precode[(bits.buffer & low_bits(self.precode_bits)) as usize];
                let symbol = entry >> VALUE_SHIFT;
                let (repeat_bits, repeat_base) = match symbol {
                    16 => (2, 3),
                    17 => (3, 3),
                    18 => (7, 11),
                    _ => (0, 0),
                };
                let code_bits = entry & STEP_BITS_MASK;
                if code_bits + repeat_bits <= bits.count {
                    break (symbol, code_bits, repeat_bits, repeat_base);
                }
                if !bits.pull(input, position) {
                    return Ok(false);
                }
            };
            bits.skip(code_bits);
            if symbol < 16 {
                self.lengths[self.lengths_read] = symbol as u8;
                self.lengths_read += 1;
                continue;
            }
            let repeated = match symbol {
                16 => *self
                    .lengths
                    .get(self.lengths_read.wrapping_sub(1))
                    .ok_or(BAD_REPEAT)?,
                _ => 0,
            };
            let end = self.lengths_read + repeat_base + bits.take(repeat_bits) as usize;
            self.lengths
                .get_mut(self.lengths_read..end)
                .filter(|_| end <= total)
                .ok_or(BAD_REPEAT)?
                .fill(repeated);
            self.lengths_read = end;
        }
        if self.lengths[END_OF_BLOCK_SYMBOL] == 0 {
            return Err(Invalid("Deflate block has no end-of-block code"));
        }
        let (litlen, distance) = self.lengths.split_at(self.litlen_codes);
        self.tables
            .build(litlen, &distance[..self.distance_codes])?;
        Ok(true)
    }
}

pub(crate) struct Inflate<'w> {
    window: &'w mut [u8],
    bits: Bits,
    state: State,
    last_block: bool,
    dynamic: Option<Box<Dynamic>>,
    uses_dynamic: bool,
    filled: usize,
    total_in: u64,
}

impl<'w> Inflate<'w> {
    pub(crate) fn new(window: &'w mut [u8]) -> Self {
        assert!(window.len() >= SMALLEST_WINDOW);
        Self {
            window,
            bits: Bits::default(),
            state: State::BlockHeader,
            last_block: false,
            dynamic: None,
            uses_dynamic: false,
            filled: 0,
            total_in: 0,
        }
    }

    pub(crate) fn total_in(&self) -> u64 {
        self.total_in
    }

    pub(crate) fn decode(&mut self, input: &[u8]) -> Result<Step<'_>, Invalid> {
        if self.window.len() - self.filled < OUTPUT_MARGIN {
            self.window
                .copy_within(self.filled - HISTORY..self.filled, 0);
            self.filled = HISTORY;
        }
        let start = self.filled;
        let mut position = 0;
        let finished = self
            .run(input, &mut position)
            .inspect_err(|&invalid| self.state = State::Failed(invalid))?;
        self.total_in += position as u64;
        Ok(Step {
            consumed: position,
            output: &self.window[start..self.filled],
            finished,
        })
    }

    fn after_block(&self) -> State {
        match self.last_block {
            true => State::Finished,
            false => State::BlockHeader,
        }
    }

    fn run(&mut self, input: &[u8], position: &mut usize) -> Result<bool, Invalid> {
        loop {
            match self.state {
                State::BlockHeader => {
                    if !self.bits.need(3, input, position) {
                        return Ok(false);
                    }
                    self.last_block = self.bits.take(1) == 1;
                    self.state = match self.bits.take(2) {
                        0 => {
                            debug_assert!(self.bits.count < 8);
                            self.bits = Bits::default();
                            State::StoredLength
                        }
                        1 => {
                            self.uses_dynamic = false;
                            State::Symbols
                        }
                        2 => {
                            self.uses_dynamic = true;
                            State::CodeCounts
                        }
                        _ => return Err(Invalid("Deflate block type 3 is reserved")),
                    };
                }
                State::StoredLength => {
                    if !self.bits.need(32, input, position) {
                        return Ok(false);
                    }
                    let length = self.bits.take(16);
                    // SPEC(RFC 1951 3.2.4): NLEN is the one's complement of LEN.
                    if length ^ self.bits.take(16) != 0xffff {
                        return Err(Invalid("Deflate stored block lengths disagree"));
                    }
                    self.state = State::Stored {
                        remaining: length as usize,
                    };
                }
                State::Stored { remaining: 0 } => self.state = self.after_block(),
                State::Stored { remaining } => {
                    let length = remaining
                        .min(input.len() - *position)
                        .min(self.window.len() - self.filled);
                    if length == 0 {
                        return Ok(false);
                    }
                    self.window[self.filled..self.filled + length]
                        .copy_from_slice(&input[*position..*position + length]);
                    *position += length;
                    self.filled += length;
                    self.state = State::Stored {
                        remaining: remaining - length,
                    };
                }
                State::CodeCounts => {
                    if !self.bits.need(14, input, position) {
                        return Ok(false);
                    }
                    let dynamic = self.dynamic.get_or_insert_with(Dynamic::boxed);
                    dynamic.code_counts(&mut self.bits)?;
                    self.state = State::PrecodeLengths;
                }
                State::PrecodeLengths => {
                    let dynamic = self.dynamic.get_or_insert_with(Dynamic::boxed);
                    if !dynamic.precode(&mut self.bits, input, position)? {
                        return Ok(false);
                    }
                    self.state = State::CodeLengths;
                }
                State::CodeLengths => {
                    let dynamic = self.dynamic.get_or_insert_with(Dynamic::boxed);
                    if !dynamic.codes(&mut self.bits, input, position)? {
                        return Ok(false);
                    }
                    self.state = State::Symbols;
                }
                State::Symbols => {
                    if self.window.len() - self.filled < OUTPUT_MARGIN {
                        return Ok(false);
                    }
                    let tables: &Tables = match &self.dynamic {
                        Some(dynamic) if self.uses_dynamic => &dynamic.tables,
                        _ => fixed_tables(),
                    };
                    if input.len() - *position >= FAST_INPUT {
                        let cursor = Cursor {
                            bits: self.bits,
                            position: *position,
                            filled: self.filled,
                        };
                        let (cursor, pause) = fast(tables, input, self.window, cursor);
                        self.bits = cursor.bits;
                        *position = cursor.position;
                        self.filled = cursor.filled;
                        self.bits.give_back_whole_bytes(position);
                        if let Pause::EndOfBlock = pause? {
                            self.state = self.after_block();
                        }
                        continue;
                    }
                    let (symbol, used) = loop {
                        if let Some(decoded) = symbol(tables, self.bits)? {
                            break decoded;
                        }
                        if !self.bits.pull(input, position) {
                            return Ok(false);
                        }
                    };
                    self.bits.skip(used);
                    match symbol {
                        Symbol::Literal(byte) => {
                            self.window[self.filled] = byte;
                            self.filled += 1;
                        }
                        Symbol::EndOfBlock => self.state = self.after_block(),
                        Symbol::Match { length, distance } => {
                            if distance > self.filled {
                                return Err(TOO_FAR_BACK);
                            }
                            copy_match(self.window, self.filled, distance, length);
                            self.filled += length;
                        }
                    }
                }
                State::Finished => {
                    debug_assert!(self.bits.count < 8);
                    return Ok(true);
                }
                State::Failed(invalid) => return Err(invalid),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[derive(Debug, PartialEq, Eq, Clone)]
    enum Outcome {
        Decoded { bytes: Vec<u8>, consumed: u64 },
        Refused,
    }

    fn zlib(stream: &[u8]) -> Outcome {
        let mut inflater = flate2::Decompress::new(false);
        let mut bytes = Vec::new();
        loop {
            bytes.reserve(64 * 1024);
            let before = (inflater.total_in(), inflater.total_out());
            let rest = &stream[inflater.total_in() as usize..];
            match inflater.decompress_vec(rest, &mut bytes, flate2::FlushDecompress::None) {
                Err(_) => return Outcome::Refused,
                Ok(flate2::Status::StreamEnd) => {
                    return Outcome::Decoded {
                        bytes,
                        consumed: inflater.total_in(),
                    };
                }
                Ok(_) if before == (inflater.total_in(), inflater.total_out()) => {
                    return Outcome::Refused;
                }
                Ok(_) => {}
            }
        }
    }

    fn ours(stream: &[u8], piece: usize, window: usize) -> Outcome {
        let mut window = vec![0xa5u8; window];
        let mut inflate = Inflate::new(&mut window);
        let mut bytes = Vec::new();
        let mut at = 0usize;
        loop {
            let end = at.saturating_add(piece).min(stream.len());
            let Ok(step) = inflate.decode(&stream[at..end]) else {
                return Outcome::Refused;
            };
            at += step.consumed;
            bytes.extend_from_slice(step.output);
            let (finished, needs_input) = (step.finished, step.needs_input());
            assert_eq!(inflate.total_in(), at as u64);
            if finished {
                return Outcome::Decoded {
                    bytes,
                    consumed: at as u64,
                };
            }
            if needs_input {
                assert_eq!(at, end, "a call with input left made no progress");
                if end == stream.len() {
                    return Outcome::Refused;
                }
            }
        }
    }

    const PIECES: [usize; 9] = [1, 2, 7, 31, 32, 33, 100, 4096, usize::MAX];
    const WINDOWS: [usize; 3] = [SMALLEST_WINDOW, SMALLEST_WINDOW + 4321, 3 * HISTORY];

    fn agrees_with_zlib(stream: &[u8], label: &str) -> Outcome {
        let expected = zlib(stream);
        for (index, piece) in PIECES.into_iter().enumerate() {
            let window = WINDOWS[index % WINDOWS.len()];
            assert!(
                ours(stream, piece, window) == expected,
                "{label}: pieces of {piece} bytes in a window of {window} differ from zlib"
            );
        }
        expected
    }

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

    fn sample(seed: u64, size: usize) -> Vec<u8> {
        let mut random = Random(seed * 2 + 1);
        let mut data = Vec::with_capacity(size);
        while data.len() < size {
            match random.below(6) {
                0 => {
                    let byte = random.next() as u8;
                    data.extend(std::iter::repeat_n(byte, 1 + random.below(600)));
                }
                1 if !data.is_empty() => {
                    let distance = 1 + random.below(data.len().min(40_000));
                    for _ in 0..3 + random.below(300) {
                        data.push(data[data.len() - distance]);
                    }
                }
                2 => data.extend((0..random.below(200)).map(|_| random.next() as u8)),
                _ => {
                    for _ in 0..random.below(40) {
                        let word = random.below(50) as u8;
                        data.extend_from_slice(&[b'a' + word % 26, b'e', b' ', b't' - word % 7]);
                    }
                }
            }
        }
        data.truncate(size);
        data
    }

    fn deflated(data: &[u8], level: u32) -> Vec<u8> {
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(level));
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn every_level_of_generated_data_decodes_as_zlib_does_in_every_piece_size() {
        for (seed, size) in [0, 1, 2, 5, 100, 1000, 65_535, 65_536, 300_000, 1_500_000]
            .into_iter()
            .enumerate()
        {
            let data = sample(seed as u64, size);
            for level in 0..=9 {
                let stream = deflated(&data, level);
                let outcome = agrees_with_zlib(&stream, &format!("{size} bytes at level {level}"));
                assert!(
                    outcome
                        == Outcome::Decoded {
                            bytes: data.clone(),
                            consumed: stream.len() as u64
                        },
                    "{size} bytes at level {level} decode to other bytes"
                );
            }
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Looked {
        NeedsBits,
        Unused,
        Symbol {
            symbol: usize,
            extra: u32,
            bits: u32,
        },
    }

    fn reference_lookup(lengths: &[u8], buffer: u64, count: u32) -> Looked {
        let codes = canonical(lengths);
        let used = lengths.iter().filter(|&&length| length != 0).count();
        if used <= 1 && (used == 0 || buffer & 1 == 1) {
            return match count {
                0 => Looked::NeedsBits,
                _ => Looked::Unused,
            };
        }
        let mut code = 0;
        for length in 1..=count.min(15) {
            code = code << 1 | (buffer >> (length - 1)) as u32 & 1;
            let Some(symbol) = codes.iter().position(|&c| c == (code, length as u8)) else {
                continue;
            };
            let extra_bits = u32::from(DISTANCE_EXTRA[symbol]);
            if length + extra_bits > count {
                return Looked::NeedsBits;
            }
            return Looked::Symbol {
                symbol,
                extra: (buffer >> length & low_bits(extra_bits)) as u32,
                bits: length + extra_bits,
            };
        }
        Looked::NeedsBits
    }

    fn table_lookup(table: &[u32], table_bits: u32, buffer: u64, count: u32) -> Looked {
        let Some((entry, skipped)) = lookup(table, table_bits, Bits { buffer, count }) else {
            return Looked::NeedsBits;
        };
        if entry & EXCEPTIONAL != 0 {
            assert_eq!(entry, UNUSED_CODE);
            return Looked::Unused;
        }
        let base = (entry >> VALUE_SHIFT) as u16;
        Looked::Symbol {
            symbol: DISTANCE_BASE.iter().position(|&b| b == base).unwrap(),
            extra: extra_bits(buffer >> skipped, entry),
            bits: skipped + (entry & STEP_BITS_MASK),
        }
    }

    #[test]
    fn every_small_code_is_built_or_refused_and_read_as_a_bit_at_a_time_reference_reads_it() {
        const SYMBOLS: usize = 7;
        const LONGEST: u8 = 5;
        let mut accepted = 0;
        let mut lengths = [0u8; SYMBOLS];
        'codes: loop {
            let filled: u32 = lengths
                .iter()
                .filter(|&&length| length != 0)
                .map(|&length| 1 << (LONGEST - length))
                .sum();
            let used = lengths.iter().filter(|&&length| length != 0).count();
            let longest = lengths.iter().copied().max().unwrap();
            let valid = filled == 1 << LONGEST || used == 0 || (used == 1 && longest == 1);
            for table_bits in 1..=4 {
                let mut table = [0x5a5a_5a5au32; 80];
                let built = build_table(&mut table, &lengths, table_bits, Alphabet::Distance);
                assert_eq!(built.is_ok(), valid, "{lengths:?} in {table_bits} bits");
                let Ok(table_bits) = built else {
                    continue;
                };
                assert!(table_bits <= u32::from(longest).max(1), "{lengths:?}");
                accepted += 1;
                for buffer in 0..1u64 << 7 {
                    for count in 0..=7 {
                        assert_eq!(
                            table_lookup(&table, table_bits, buffer, count),
                            reference_lookup(&lengths, buffer & low_bits(count), count),
                            "{lengths:?} in {table_bits} bits: {buffer:07b} with {count} bits"
                        );
                    }
                }
            }
            for length in &mut lengths {
                if *length < LONGEST {
                    *length += 1;
                    continue 'codes;
                }
                *length = 0;
            }
            break;
        }
        assert!(accepted > 10_000, "{accepted}");
    }

    #[test]
    fn random_whole_alphabets_decode_every_codeword_to_its_symbol() {
        let mut random = Random(31);
        let mut subtables = 0;
        for round in 0..3000 {
            let (alphabet, symbols, table_bits, entries) = match round % 2 {
                0 => (Alphabet::LitLen, 286, LITLEN_BITS, LITLEN_ENTRIES),
                _ => (Alphabet::Distance, 30, DISTANCE_BITS, DISTANCE_ENTRIES),
            };
            let used = 2 + random.below(symbols - 1);
            let longest = [2, 8, 9, 11, 12, 13, 15][random.below(7)];
            let used = some_of(&mut random, symbols, used, &[]);
            let lengths = random_code(&mut random, symbols, &used, longest);
            let mut table = vec![0u32; entries];
            let table_bits = build_table(&mut table, &lengths, table_bits, alphabet).unwrap();
            for (symbol, &(code, length)) in canonical(&lengths).iter().enumerate() {
                if length == 0 {
                    continue;
                }
                let reversed = u64::from(code.reverse_bits() >> (32 - u32::from(length)));
                let expected = alphabet.symbols()[symbol] & !STEP_BITS_MASK;
                for padding in [0, u64::MAX] {
                    let buffer = reversed | padding << length;
                    let bits = Bits { buffer, count: 48 };
                    let (entry, skipped) = lookup(&table, table_bits, bits).unwrap();
                    let code_bits = (entry >> CODE_BITS_SHIFT) & CODE_BITS_MASK;
                    assert_eq!(skipped + code_bits, u32::from(length), "round {round}");
                    assert_eq!(
                        entry & !(STEP_BITS_MASK | CODE_BITS_MASK << CODE_BITS_SHIFT),
                        expected,
                        "round {round} symbol {symbol}"
                    );
                    subtables += usize::from(skipped != 0);
                }
            }
        }
        assert!(subtables > 10_000);
    }

    #[test]
    fn every_match_copy_equals_a_byte_at_a_time_copy() {
        let mut window = vec![0u8; 700];
        let mut expected = vec![0u8; 700];
        for distance in (1..=70).chain([255, 256, 257, 258, 259, 300]) {
            for length in 3..=LONGEST_MATCH {
                for (index, byte) in window.iter_mut().enumerate() {
                    *byte = (index * 31 + index / 7 + distance) as u8;
                }
                expected.copy_from_slice(&window);
                let at = distance.max(3) + distance % 5;
                for index in at..at + length {
                    expected[index] = expected[index - distance];
                }
                copy_match(&mut window, at, distance, length);
                let written = at + length;
                assert!(
                    window[..written] == expected[..written],
                    "distance {distance}, length {length}"
                );
                assert!(
                    window[written + COPY_OVERRUN..] == expected[written + COPY_OVERRUN..],
                    "distance {distance}, length {length} wrote too far"
                );
            }
        }
    }

    #[test]
    fn the_decoder_keeps_its_tables_off_the_callers_stack() {
        assert!(
            std::mem::size_of::<Inflate>() <= 128,
            "{}",
            std::mem::size_of::<Inflate>()
        );
    }

    #[test]
    fn a_refused_stream_stays_refused_with_the_same_error() {
        let mut stream = deflated(&sample(11, 5000), 6);
        stream[0] |= 0b110;
        let mut window = vec![0u8; SMALLEST_WINDOW];
        let mut inflate = Inflate::new(&mut window);
        let first = inflate.decode(&stream).err().unwrap();
        assert_eq!(first, Invalid("Deflate block type 3 is reserved"));
        for input in [&stream[1..], &[]] {
            assert_eq!(inflate.decode(input).err(), Some(first));
        }
    }

    #[test]
    fn bytes_after_the_last_block_are_left_unconsumed() {
        for level in [0, 1, 6] {
            let data = sample(7, 50_000);
            let mut stream = deflated(&data, level);
            let length = stream.len() as u64;
            stream.extend_from_slice(&[0xff; 40]);
            let outcome = agrees_with_zlib(&stream, "trailing bytes");
            assert!(
                outcome
                    == Outcome::Decoded {
                        bytes: data,
                        consumed: length
                    }
            );
        }
    }

    #[test]
    fn every_entry_of_the_zip_test_archives_decodes_as_zlib_does() {
        use std::io::Read;
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let (mut decoded, mut refused) = (0, 0);
        for file in walkdir::WalkDir::new(data).sort_by_file_name() {
            let path = file.unwrap().into_path();
            if path.extension().is_none_or(|extension| extension != "zip") {
                continue;
            }
            let Ok(mut archive) = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()) else {
                continue;
            };
            for index in 0..archive.len() {
                let Ok(mut entry) = archive.by_index_raw(index) else {
                    continue;
                };
                if entry.compression() != zip::CompressionMethod::Deflated {
                    continue;
                }
                let mut packed = Vec::new();
                if entry.read_to_end(&mut packed).is_err() {
                    continue;
                }
                let label = format!("{} entry {index}", path.display());
                match agrees_with_zlib(&packed, &label) {
                    Outcome::Decoded { .. } => decoded += 1,
                    Outcome::Refused => refused += 1,
                }
            }
        }
        assert!(decoded > 100 && refused > 0, "{decoded} {refused}");
    }

    #[test]
    fn the_silesia_files_at_three_levels_decode_as_zlib_does() {
        let Some(folder) = std::env::var_os("ZIPPER_SILESIA") else {
            return;
        };
        let mut checked = 0;
        for file in std::fs::read_dir(folder).unwrap() {
            let path = file.unwrap().path();
            if !path.is_file() {
                continue;
            }
            let data = std::fs::read(&path).unwrap();
            for level in [1, 6, 9] {
                let stream = deflated(&data, level);
                let label = format!("{} at level {level}", path.display());
                let outcome = agrees_with_zlib(&stream, &label);
                assert!(
                    outcome
                        == Outcome::Decoded {
                            bytes: data.clone(),
                            consumed: stream.len() as u64
                        },
                    "{label} decodes to other bytes"
                );
            }
            checked += 1;
        }
        assert!(checked >= 12, "{checked}");
    }

    #[derive(Default)]
    struct BitWriter {
        bytes: Vec<u8>,
        buffer: u64,
        count: u32,
    }

    impl BitWriter {
        fn put(&mut self, value: u32, count: u32) {
            assert!(count <= 32 && u64::from(value) < 1 << count);
            self.buffer |= u64::from(value) << self.count;
            self.count += count;
            while self.count >= 8 {
                self.bytes.push(self.buffer as u8);
                self.buffer >>= 8;
                self.count -= 8;
            }
        }

        fn code(&mut self, (code, length): (u32, u8)) {
            assert!(length > 0, "the symbol has no code");
            for bit in (0..length).rev() {
                self.put((code >> bit) & 1, 1);
            }
        }

        fn align(&mut self) {
            if self.count > 0 {
                self.put(0, 8 - self.count);
            }
        }

        fn finish(mut self) -> Vec<u8> {
            self.align();
            self.bytes
        }
    }

    fn canonical(lengths: &[u8]) -> Vec<(u32, u8)> {
        let mut code = 0u32;
        let mut codes = vec![(0, 0); lengths.len()];
        for length in 1..=15u8 {
            for (symbol, _) in lengths.iter().enumerate().filter(|&(_, &l)| l == length) {
                codes[symbol] = (code, length);
                code += 1;
            }
            code <<= 1;
        }
        codes
    }

    fn fixed_lengths() -> (Vec<u8>, Vec<u8>) {
        let mut litlen = vec![8u8; 288];
        litlen[144..256].fill(9);
        litlen[256..280].fill(7);
        (litlen, vec![5; 32])
    }

    #[derive(Clone, Copy, Debug)]
    enum Token {
        Literal(u8),
        Match {
            length_code: usize,
            length_extra: u32,
            distance_code: usize,
            distance_extra: u32,
        },
    }

    fn put_tokens(writer: &mut BitWriter, tokens: &[Token], litlen: &[u8], distance: &[u8]) {
        let (litlen, distance) = (canonical(litlen), canonical(distance));
        for &token in tokens {
            match token {
                Token::Literal(byte) => writer.code(litlen[usize::from(byte)]),
                Token::Match {
                    length_code,
                    length_extra,
                    distance_code,
                    distance_extra,
                } => {
                    writer.code(litlen[257 + length_code]);
                    writer.put(length_extra, u32::from(LENGTH_EXTRA[length_code]));
                    writer.code(distance[distance_code]);
                    writer.put(distance_extra, u32::from(DISTANCE_EXTRA[distance_code]));
                }
            }
        }
        writer.code(litlen[256]);
    }

    fn whole_precode() -> [u8; 19] {
        let mut lengths = [4u8; 19];
        lengths[13..].fill(5);
        lengths
    }

    fn put_code_counts(writer: &mut BitWriter, litlen: usize, distance: usize, precode: &[u8; 19]) {
        writer.put(litlen as u32 - 257, 5);
        writer.put(distance as u32 - 1, 5);
        writer.put(19 - 4, 4);
        for symbol in PRECODE_ORDER {
            writer.put(u32::from(precode[usize::from(symbol)]), 3);
        }
    }

    fn put_dynamic_header(
        writer: &mut BitWriter,
        litlen: &[u8],
        distance: &[u8],
        random: &mut Random,
    ) {
        let lengths: Vec<u8> = litlen.iter().chain(distance).copied().collect();
        let precode_lengths = whole_precode();
        let precode = canonical(&precode_lengths);
        put_code_counts(writer, litlen.len(), distance.len(), &precode_lengths);
        let mut at = 0;
        while at < lengths.len() {
            let run = lengths[at..]
                .iter()
                .take_while(|&&l| l == lengths[at])
                .count();
            let repeats = random.below(2) == 0;
            if repeats && lengths[at] == 0 && run >= 11 {
                let count = 11 + random.below(run.min(138) - 10);
                writer.code(precode[18]);
                writer.put(count as u32 - 11, 7);
                at += count;
            } else if repeats && lengths[at] == 0 && run >= 3 {
                let count = 3 + random.below(run.min(10) - 2);
                writer.code(precode[17]);
                writer.put(count as u32 - 3, 3);
                at += count;
            } else if repeats && at > 0 && lengths[at - 1] == lengths[at] && run >= 3 {
                let count = 3 + random.below(run.min(6) - 2);
                writer.code(precode[16]);
                writer.put(count as u32 - 3, 2);
                at += count;
            } else {
                writer.code(precode[usize::from(lengths[at])]);
                at += 1;
            }
        }
    }

    fn random_code(random: &mut Random, symbols: usize, used: &[usize], longest: u8) -> Vec<u8> {
        let mut depths = vec![1u8, 1];
        while depths.len() < used.len() {
            if depths.iter().all(|&depth| depth == longest) {
                break;
            }
            let leaf = random.below(depths.len());
            if depths[leaf] < longest {
                depths[leaf] += 1;
                depths.push(depths[leaf]);
            }
        }
        let mut lengths = vec![0u8; symbols];
        for (&symbol, &depth) in used.iter().zip(&depths) {
            lengths[symbol] = depth;
        }
        lengths
    }

    fn some_of(random: &mut Random, from: usize, count: usize, always: &[usize]) -> Vec<usize> {
        let mut pool: Vec<usize> = (0..from).filter(|at| !always.contains(at)).collect();
        let mut chosen = always.to_vec();
        while chosen.len() < count && !pool.is_empty() {
            chosen.push(pool.swap_remove(random.below(pool.len())));
        }
        chosen
    }

    fn random_tokens(
        random: &mut Random,
        litlen: &[u8],
        distance: &[u8],
        count: usize,
        produced: &mut Vec<u8>,
    ) -> Vec<Token> {
        let literals: Vec<usize> = (0..256).filter(|&s| litlen[s] != 0).collect();
        let lengths: Vec<usize> = (0..29)
            .filter(|&s| litlen.get(257 + s).is_some_and(|&l| l != 0))
            .collect();
        let distances: Vec<usize> = (0..30)
            .filter(|&s| distance.get(s).is_some_and(|&l| l != 0))
            .collect();
        let mut tokens = Vec::new();
        for _ in 0..count {
            let wants_match = random.below(3) == 0 || literals.is_empty();
            if wants_match && !lengths.is_empty() && !distances.is_empty() {
                let length_code = lengths[random.below(lengths.len())];
                let distance_code = distances[random.below(distances.len())];
                let length_extra = random.below(1 << LENGTH_EXTRA[length_code]) as u32;
                let distance_extra = random.below(1 << DISTANCE_EXTRA[distance_code]) as u32;
                let back = usize::from(DISTANCE_BASE[distance_code]) + distance_extra as usize;
                if back <= produced.len() {
                    let length = usize::from(LENGTH_BASE[length_code]) + length_extra as usize;
                    for _ in 0..length {
                        produced.push(produced[produced.len() - back]);
                    }
                    tokens.push(Token::Match {
                        length_code,
                        length_extra,
                        distance_code,
                        distance_extra,
                    });
                    continue;
                }
            }
            if !literals.is_empty() {
                let byte = literals[random.below(literals.len())] as u8;
                produced.push(byte);
                tokens.push(Token::Literal(byte));
            }
        }
        tokens
    }

    fn put_random_block(
        writer: &mut BitWriter,
        random: &mut Random,
        last: bool,
        produced: &mut Vec<u8>,
    ) {
        writer.put(u32::from(last), 1);
        match random.below(8) {
            0 => {
                writer.put(0, 2);
                writer.align();
                let length = [0, 1, 5, 300, 65_535][random.below(5)];
                writer.put(length, 16);
                writer.put(length ^ 0xffff, 16);
                for _ in 0..length {
                    let byte = random.next() as u8;
                    writer.put(u32::from(byte), 8);
                    produced.push(byte);
                }
            }
            1 | 2 => {
                writer.put(1, 2);
                let (litlen, distance) = fixed_lengths();
                let count = [0, 1, 40, 4000][random.below(4)];
                let tokens = random_tokens(random, &litlen[..286], &distance, count, produced);
                put_tokens(writer, &tokens, &litlen, &distance);
            }
            _ => {
                writer.put(2, 2);
                let litlen_codes = 257 + random.below(30);
                let distance_codes = 1 + random.below(30);
                let longest = [2, 7, 9, 11, 12, 15][random.below(6)];
                let used = [2, 3, 20, 150, 286][random.below(5)].min(litlen_codes);
                let used = some_of(random, litlen_codes, used, &[256]);
                let litlen = random_code(random, litlen_codes, &used, longest);
                let used = [0, 1, 2, 8, 30][random.below(5)].min(distance_codes);
                let used = some_of(random, distance_codes, used, &[]);
                let longest = [1, 5, 8, 9, 15][random.below(5)];
                let distance = match used.len() {
                    0 => vec![0; distance_codes],
                    1 => (0..distance_codes)
                        .map(|s| u8::from(s == used[0]))
                        .collect(),
                    _ => random_code(random, distance_codes, &used, longest),
                };
                put_dynamic_header(writer, &litlen, &distance, random);
                let count = [0, 1, 40, 4000][random.below(4)];
                let tokens = random_tokens(random, &litlen, &distance, count, produced);
                put_tokens(writer, &tokens, &litlen, &distance);
            }
        }
    }

    fn random_stream(random: &mut Random, blocks: usize) -> (Vec<u8>, Vec<u8>) {
        let mut writer = BitWriter::default();
        let mut produced = Vec::new();
        for block in 0..blocks {
            put_random_block(&mut writer, random, block + 1 == blocks, &mut produced);
        }
        (writer.finish(), produced)
    }

    #[test]
    fn random_blocks_of_every_type_with_random_codes_decode_as_zlib_and_as_built() {
        let mut random = Random(0x9e3779b97f4a7c15);
        let mut long_streams = 0;
        for round in 0..1500 {
            let blocks = 1 + random.below(4);
            let (stream, produced) = random_stream(&mut random, blocks);
            let outcome = agrees_with_zlib(&stream, &format!("round {round}"));
            assert!(
                outcome
                    == Outcome::Decoded {
                        bytes: produced,
                        consumed: stream.len() as u64
                    },
                "round {round} decodes to other bytes than it was built from"
            );
            long_streams += usize::from(stream.len() > 2000);
        }
        assert!(long_streams > 100);
    }

    #[test]
    fn every_cut_of_small_streams_is_refused_as_zlib_refuses_it() {
        let mut random = Random(77);
        for round in 0..40 {
            let (stream, _) = random_stream(&mut random, 3);
            let stream = &stream[..stream.len().min(700)];
            for cut in 0..stream.len() {
                agrees_with_zlib(&stream[..cut], &format!("round {round} cut at {cut}"));
            }
        }
        let stream = deflated(&sample(3, 20_000), 6);
        for cut in 0..stream.len() {
            assert_eq!(
                agrees_with_zlib(&stream[..cut], &format!("level 6 cut at {cut}")),
                Outcome::Refused
            );
        }
    }

    #[test]
    fn bit_flips_and_random_bytes_decode_or_fail_as_zlib_does() {
        let mut random = Random(4242);
        let (mut decoded, mut refused) = (0, 0);
        let mut count = |outcome: Outcome| match outcome {
            Outcome::Decoded { .. } => decoded += 1,
            Outcome::Refused => refused += 1,
        };
        for round in 0..60 {
            let (stream, _) = random_stream(&mut random, 2);
            for bit in 0..stream.len().min(400) * 8 {
                let mut flipped = stream.clone();
                flipped[bit / 8] ^= 1 << (bit % 8);
                count(agrees_with_zlib(
                    &flipped,
                    &format!("round {round} bit {bit}"),
                ));
            }
            for _ in 0..200 {
                let mut flipped = stream.clone();
                for _ in 0..1 + random.below(3) {
                    let bit = random.below(stream.len() * 8);
                    flipped[bit / 8] ^= 1 << (bit % 8);
                }
                count(agrees_with_zlib(&flipped, &format!("round {round} flips")));
            }
        }
        for level in [1, 6, 9] {
            let stream = deflated(&sample(level.into(), 30_000), level);
            for bit in 0..stream.len().min(1500) * 8 {
                let mut flipped = stream.clone();
                flipped[bit / 8] ^= 1 << (bit % 8);
                count(agrees_with_zlib(
                    &flipped,
                    &format!("level {level} bit {bit}"),
                ));
            }
        }
        for round in 0..20_000 {
            let mut noise: Vec<u8> = (0..random.below(300))
                .map(|_| random.next() as u8)
                .collect();
            if let Some(first) = noise.first_mut() {
                *first = (*first & !6) | [2, 4, 4, 4][random.below(4)];
            }
            count(agrees_with_zlib(&noise, &format!("noise round {round}")));
        }
        assert!(decoded > 1000 && refused > 10_000, "{decoded} {refused}");
    }

    #[test]
    fn the_longest_codes_back_to_back_decode_as_zlib_does_at_every_bit_offset() {
        let mut litlen = vec![0u8; 286];
        litlen[usize::from(b'x')] = 1;
        for length in (2..=14).filter(|&length| length != 11) {
            litlen[length] = length as u8;
        }
        litlen[usize::from(b'a')] = 11;
        litlen[256] = 15;
        litlen[284] = 15;
        let mut distance = vec![0u8; 30];
        for length in (1..=14).filter(|&length| length != 8) {
            distance[length - 1] = length as u8;
        }
        distance[28] = 8;
        distance[27] = 15;
        distance[29] = 15;
        let far_match = |round: usize, distance_code: usize| Token::Match {
            length_code: 284 - 257,
            length_extra: (round % 32) as u32,
            distance_code,
            distance_extra: (8191 - round % 3) as u32,
        };
        for offset in 0..8 {
            let mut tokens = vec![Token::Literal(b'x'); 33_000 + offset];
            for round in 0..300 {
                let literals = [2, 1, 0, 3, 2][round % 5];
                tokens.extend(std::iter::repeat_n(Token::Literal(b'a'), literals));
                tokens.push(far_match(round, 28 + round % 2));
                if round % 7 == 0 {
                    tokens.push(far_match(round, 29));
                }
            }
            let mut produced = Vec::new();
            for &token in &tokens {
                match token {
                    Token::Literal(byte) => produced.push(byte),
                    Token::Match {
                        length_code,
                        length_extra,
                        distance_code,
                        distance_extra,
                    } => {
                        let back =
                            usize::from(DISTANCE_BASE[distance_code]) + distance_extra as usize;
                        let length = usize::from(LENGTH_BASE[length_code]) + length_extra as usize;
                        for _ in 0..length {
                            produced.push(produced[produced.len() - back]);
                        }
                    }
                }
            }
            let mut writer = BitWriter::default();
            writer.put(0b101, 3);
            put_dynamic_header(&mut writer, &litlen, &distance, &mut Random(3));
            put_tokens(&mut writer, &tokens, &litlen, &distance);
            let stream = writer.finish();
            let outcome = agrees_with_zlib(&stream, &format!("offset {offset}"));
            assert!(
                outcome
                    == Outcome::Decoded {
                        bytes: produced,
                        consumed: stream.len() as u64
                    },
                "offset {offset} decodes to other bytes than it was built from"
            );
        }
    }

    fn put_stored(writer: &mut BitWriter, length: usize, last: bool) {
        writer.put(u32::from(last), 1);
        writer.put(0, 2);
        writer.align();
        writer.put(length as u32, 16);
        writer.put(length as u32 ^ 0xffff, 16);
        for index in 0..length {
            writer.put(index as u32 * 7 % 256, 8);
        }
    }

    #[test]
    fn stored_fixed_and_dynamic_blocks_meet_at_every_bit_offset_and_length() {
        let mut random = Random(5);
        let (litlen, distance) = fixed_lengths();
        for length in [0, 1, 2, 31, 32, 33, 276, 277, 278, 65_534, 65_535] {
            for literals_before in 0..8 {
                let mut writer = BitWriter::default();
                if literals_before > 0 {
                    writer.put(0, 1);
                    writer.put(1, 2);
                    let run = vec![Token::Literal(b'x'); literals_before];
                    put_tokens(&mut writer, &run, &litlen, &distance);
                }
                put_stored(&mut writer, length, false);
                writer.put(0, 1);
                writer.put(2, 2);
                let code = random_code(&mut random, 257, &[b'y'.into(), 256], 15);
                put_dynamic_header(&mut writer, &code, &[0], &mut random);
                put_tokens(&mut writer, &[Token::Literal(b'y')], &code, &[0]);
                writer.put(0, 1);
                writer.put(1, 2);
                put_tokens(&mut writer, &[], &litlen, &distance);
                put_stored(&mut writer, length, true);
                let stream = writer.finish();
                let label = format!("stored length {length} after {literals_before} literals");
                let Outcome::Decoded { bytes, consumed } = agrees_with_zlib(&stream, &label) else {
                    panic!("{label} is refused");
                };
                assert_eq!(consumed, stream.len() as u64, "{label}");
                assert_eq!(bytes.len(), 2 * length + literals_before + 1, "{label}");
            }
        }
    }

    fn outcome_of(build: impl Fn(&mut BitWriter)) -> Outcome {
        let mut writer = BitWriter::default();
        build(&mut writer);
        let mut stream = writer.finish();
        stream.extend_from_slice(&[0; 64]);
        agrees_with_zlib(&stream, "a hand-built stream")
    }

    #[test]
    fn fixed_blocks_with_codes_outside_the_format_are_refused() {
        let (litlen, _) = fixed_lengths();
        let fixed = canonical(&litlen);
        let after_one_literal = |tail: &dyn Fn(&mut BitWriter)| {
            outcome_of(|writer| {
                writer.put(0b011, 3);
                writer.code(fixed[usize::from(b'a')]);
                tail(writer);
            })
        };
        assert_eq!(outcome_of(|writer| writer.put(0b111, 3)), Outcome::Refused);
        for symbol in [286, 287] {
            assert_eq!(
                after_one_literal(&|writer| writer.code(fixed[symbol])),
                Outcome::Refused
            );
        }
        for distance in [30u32, 31] {
            let outcome = after_one_literal(&|writer| {
                writer.code(fixed[257]);
                writer.code((distance, 5));
            });
            assert_eq!(outcome, Outcome::Refused);
        }
        let too_far = after_one_literal(&|writer| {
            writer.code(fixed[257]);
            writer.code((1, 5));
        });
        assert_eq!(too_far, Outcome::Refused);
        let reaches_the_start = after_one_literal(&|writer| {
            writer.code(fixed[257]);
            writer.code((0, 5));
            writer.code(fixed[256]);
        });
        assert!(
            reaches_the_start
                == Outcome::Decoded {
                    bytes: b"aaaa".to_vec(),
                    consumed: 4
                }
        );
    }

    #[test]
    fn stored_blocks_whose_lengths_disagree_are_refused() {
        let outcome = outcome_of(|writer| {
            writer.put(0b001, 3);
            writer.align();
            writer.put(5, 16);
            writer.put(5, 16);
        });
        assert_eq!(outcome, Outcome::Refused);
    }

    fn dynamic(litlen: &[u8], distance: &[u8], tail: impl Fn(&mut BitWriter)) -> Outcome {
        outcome_of(|writer| {
            writer.put(0b101, 3);
            put_dynamic_header(writer, litlen, distance, &mut Random(1));
            tail(writer);
        })
    }

    fn lengths_of(symbols: usize, codes: &[(usize, u8)]) -> Vec<u8> {
        let mut lengths = vec![0u8; symbols];
        for &(symbol, length) in codes {
            lengths[symbol] = length;
        }
        lengths
    }

    #[test]
    fn dynamic_codes_are_accepted_and_refused_as_zlib_does() {
        let two = lengths_of(257, &[(65, 1), (256, 1)]);
        let literal_then_end = |writer: &mut BitWriter| {
            writer.put(0, 1);
            writer.put(1, 1);
        };
        assert!(matches!(
            dynamic(&two, &[0], literal_then_end),
            Outcome::Decoded { ref bytes, .. } if bytes == b"A"
        ));
        let only_the_end = lengths_of(257, &[(256, 1)]);
        assert!(matches!(
            dynamic(&only_the_end, &[0], |writer| writer.put(0, 1)),
            Outcome::Decoded { ref bytes, .. } if bytes.is_empty()
        ));
        assert_eq!(
            dynamic(&only_the_end, &[0], |writer| writer.put(1, 1)),
            Outcome::Refused,
            "the unused half of a one-code table"
        );
        let no_end = lengths_of(257, &[(65, 1), (66, 1)]);
        assert_eq!(dynamic(&no_end, &[0], |_| {}), Outcome::Refused);
        let mut writer = BitWriter::default();
        writer.put(0b101, 3);
        put_dynamic_header(&mut writer, &no_end, &[0], &mut Random(1));
        let mut window = vec![0u8; SMALLEST_WINDOW];
        assert_eq!(
            Inflate::new(&mut window).decode(&writer.finish()).err(),
            Some(Invalid("Deflate block has no end-of-block code")),
            "a block that cannot end is refused before it decodes anything"
        );
        let oversubscribed = lengths_of(257, &[(65, 1), (66, 1), (256, 1)]);
        assert_eq!(dynamic(&oversubscribed, &[0], |_| {}), Outcome::Refused);
        let incomplete = lengths_of(257, &[(65, 2), (256, 2)]);
        assert_eq!(dynamic(&incomplete, &[0], |_| {}), Outcome::Refused);
        let one_long_end = lengths_of(257, &[(256, 2)]);
        assert_eq!(dynamic(&one_long_end, &[0], |_| {}), Outcome::Refused);

        let with_length = lengths_of(258, &[(65, 2), (256, 2), (257, 1)]);
        let a_match = |distance_bit: u32| {
            move |writer: &mut BitWriter| {
                writer.put(0b01, 2);
                writer.put(0, 1);
                writer.put(distance_bit, 1);
                writer.put(0b11, 2);
            }
        };
        assert!(matches!(
            dynamic(&with_length, &[1], a_match(0)),
            Outcome::Decoded { ref bytes, .. } if bytes == b"AAAA"
        ));
        assert_eq!(
            dynamic(&with_length, &[1], a_match(1)),
            Outcome::Refused,
            "the unused half of a one-code distance table"
        );
        assert_eq!(
            dynamic(&with_length, &[0], a_match(0)),
            Outcome::Refused,
            "a distance from a block with no distance code"
        );
        assert_eq!(
            dynamic(&with_length, &[2], |_| {}),
            Outcome::Refused,
            "one distance code of 2 bits"
        );
        assert_eq!(
            dynamic(&with_length, &[1, 1, 1], |_| {}),
            Outcome::Refused,
            "three distance codes of 1 bit"
        );
    }

    #[test]
    fn dynamic_headers_outside_the_format_are_refused() {
        let precode = whole_precode();
        let codes = canonical(&precode);
        let complete = |symbols: usize, shorter: u8| -> Vec<u8> {
            let longer_from = (2 << shorter) - symbols;
            (0..symbols)
                .map(|symbol| shorter + u8::from(symbol >= longer_from))
                .collect()
        };
        for (litlen, distance, accepted) in [
            (286, 30, true),
            (287, 30, false),
            (288, 30, false),
            (286, 31, false),
            (286, 32, false),
        ] {
            let lengths = (complete(litlen, 8), complete(distance, 4));
            let outcome = outcome_of(|writer| {
                writer.put(0b101, 3);
                put_code_counts(writer, litlen, distance, &precode);
                for &length in lengths.0.iter().chain(&lengths.1) {
                    writer.code(codes[usize::from(length)]);
                }
                writer.code(canonical(&lengths.0)[256]);
            });
            assert_eq!(
                matches!(outcome, Outcome::Decoded { .. }),
                accepted,
                "{litlen} and {distance} codes"
            );
        }
        let repeat_without_a_length = outcome_of(|writer| {
            writer.put(0b101, 3);
            put_code_counts(writer, 257, 1, &precode);
            writer.code(codes[16]);
            writer.put(0, 2);
        });
        assert_eq!(repeat_without_a_length, Outcome::Refused);
        let repeat_past_the_end = outcome_of(|writer| {
            writer.put(0b101, 3);
            put_code_counts(writer, 257, 1, &precode);
            for _ in 0..2 {
                writer.code(codes[18]);
                writer.put(127, 7);
            }
        });
        assert_eq!(repeat_past_the_end, Outcome::Refused);
        for lengths in [[0u8; 19], [1; 19]] {
            let outcome = outcome_of(|writer| {
                writer.put(0b101, 3);
                put_code_counts(writer, 257, 1, &lengths);
            });
            assert_eq!(outcome, Outcome::Refused);
        }
        let mut one_code = [0u8; 19];
        one_code[0] = 1;
        let incomplete_precode = outcome_of(|writer| {
            writer.put(0b101, 3);
            put_code_counts(writer, 257, 1, &one_code);
        });
        assert_eq!(incomplete_precode, Outcome::Refused);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(8)]
    fn pulled_bits_are_the_inputs_next_bytes_and_unused_whole_bytes_go_back() {
        let bytes: [u8; 6] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let input = &bytes[..length];
        let count: u32 = kani::any();
        kani::assume(count < MOST_BITS_IN_ONE_SYMBOL);
        let buffer: u64 = kani::any();
        kani::assume(buffer <= low_bits(count));
        let wanted: u32 = kani::any();
        kani::assume(wanted <= MOST_BITS_IN_ONE_SYMBOL);

        let mut bits = Bits { buffer, count };
        let mut position = 0;
        let enough = bits.need(wanted, input, &mut position);
        assert!(position <= length);
        assert!(bits.count == count + 8 * position as u32);
        assert!(bits.count < MOST_BITS_IN_ONE_SYMBOL + 8);
        assert!(enough == (bits.count >= wanted));
        assert!(enough || position == length);
        assert!(position == 0 || bits.count - 8 < wanted);
        assert!(bits.buffer <= low_bits(bits.count));
        assert!(bits.buffer & low_bits(count) == buffer);
        let pulled: usize = kani::any();
        kani::assume(pulled < position);
        assert!((bits.buffer >> (count + 8 * pulled as u32)) as u8 == input[pulled]);

        let taken: u32 = kani::any();
        kani::assume(taken <= 32 && taken <= bits.count);
        let before = bits;
        assert!(u64::from(bits.take(taken)) == before.buffer & low_bits(taken));
        assert!(bits.buffer == before.buffer >> taken && bits.count == before.count - taken);

        let before = (bits, position);
        bits.give_back_whole_bytes(&mut position);
        let returned = before.1 - position;
        assert!(bits.count + 8 * returned as u32 == before.0.count);
        assert!(bits.count < 8 || position == 0);
        assert!(bits.buffer == before.0.buffer & low_bits(bits.count));
    }

    #[kani::proof]
    fn the_next_reversed_code_is_the_code_after_it_with_its_bits_reversed() {
        let length: usize = kani::any();
        kani::assume(1 <= length && length <= LONGEST_CODE);
        let code: u16 = kani::any();
        kani::assume(u32::from(code) < 1 << length);
        let reversed = usize::from(code.reverse_bits() >> (16 - length));
        match next_reversed_code(reversed, length) {
            None => assert!(u32::from(code) == (1 << length) - 1),
            Some(next) => {
                assert!(next == usize::from((code + 1).reverse_bits() >> (16 - length)));
            }
        }
    }

    #[kani::proof]
    fn every_symbols_entry_keeps_its_fields_apart_and_its_value_inside_the_format() {
        let code_bits: usize = kani::any();
        kani::assume(1 <= code_bits && code_bits <= LONGEST_CODE);
        let symbol: u16 = kani::any();
        let before: u64 = kani::any();

        if usize::from(symbol) < PRECODE_SYMBOLS {
            let made = entry(Alphabet::Precode.symbols(), symbol, code_bits);
            assert!(made >> VALUE_SHIFT == u32::from(symbol));
            assert!(made & STEP_BITS_MASK == code_bits as u32);
        }
        if usize::from(symbol) < DISTANCE_SYMBOLS {
            let made = entry(Alphabet::Distance.symbols(), symbol, code_bits);
            assert!((made >> CODE_BITS_SHIFT) & CODE_BITS_MASK == code_bits as u32);
            assert!(made & (SUBTABLE | LITERAL) == 0);
            let extra = (made & STEP_BITS_MASK) - code_bits as u32;
            assert!((made & EXCEPTIONAL != 0) == (usize::from(symbol) >= MOST_DISTANCE_CODES));
            if made & EXCEPTIONAL == 0 {
                assert!(extra <= 13 && extra_bits(before, made) < 1 << extra);
                let farthest = (made >> VALUE_SHIFT) + (1 << extra) - 1;
                assert!(1 <= made >> VALUE_SHIFT && farthest as usize <= HISTORY);
            }
        }
        if usize::from(symbol) < LITLEN_SYMBOLS {
            let made = entry(Alphabet::LitLen.symbols(), symbol, code_bits);
            assert!((made >> CODE_BITS_SHIFT) & CODE_BITS_MASK == code_bits as u32);
            assert!(made & SUBTABLE == 0);
            let extra = (made & STEP_BITS_MASK) - code_bits as u32;
            let symbol = usize::from(symbol);
            assert!((made & LITERAL != 0) == (symbol < END_OF_BLOCK_SYMBOL));
            assert!((made & END_OF_BLOCK != 0) == (symbol == END_OF_BLOCK_SYMBOL));
            let is_length = symbol > END_OF_BLOCK_SYMBOL && symbol < MOST_LITLEN_CODES;
            assert!((made & (LITERAL | EXCEPTIONAL) == 0) == is_length);
            if made & LITERAL != 0 {
                assert!(extra == 0 && (made >> VALUE_SHIFT) as u8 as usize == symbol);
            }
            if is_length {
                assert!(extra <= 5 && extra_bits(before, made) < 1 << extra);
                let longest = (made >> VALUE_SHIFT) + (1 << extra) - 1;
                assert!(3 <= made >> VALUE_SHIFT && longest as usize <= LONGEST_MATCH);
            }
        }
    }
}
