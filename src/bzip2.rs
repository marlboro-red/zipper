pub(crate) const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
pub(crate) const END_MAGIC: u64 = 0x1772_4538_5090;
pub(crate) const MAGIC_BITS: u64 = 48;
pub(crate) const END_BITS: u64 = MAGIC_BITS + 32;
pub(crate) const BYTES_PER_LEVEL: usize = 100_000;
const LONGEST_CODE: u32 = 20;
const KEPT_SELECTORS: usize = 18_002;
const SYMBOLS_PER_SELECTOR: usize = 50;
const LARGEST_ALPHABET: usize = 258;
const LARGEST_RUN_BIT: u32 = 21;
const LOOKUP_BITS: u32 = 10;
const SLOW: u16 = 0;
const SCAN_STEP: usize = 32;
const RANDOM_STEPS: [u16; 512] = [
    619, 720, 127, 481, 931, 816, 813, 233, 566, 247, 985, 724, 205, 454, 863, 491, 741, 242, 949,
    214, 733, 859, 335, 708, 621, 574, 73, 654, 730, 472, 419, 436, 278, 496, 867, 210, 399, 680,
    480, 51, 878, 465, 811, 169, 869, 675, 611, 697, 867, 561, 862, 687, 507, 283, 482, 129, 807,
    591, 733, 623, 150, 238, 59, 379, 684, 877, 625, 169, 643, 105, 170, 607, 520, 932, 727, 476,
    693, 425, 174, 647, 73, 122, 335, 530, 442, 853, 695, 249, 445, 515, 909, 545, 703, 919, 874,
    474, 882, 500, 594, 612, 641, 801, 220, 162, 819, 984, 589, 513, 495, 799, 161, 604, 958, 533,
    221, 400, 386, 867, 600, 782, 382, 596, 414, 171, 516, 375, 682, 485, 911, 276, 98, 553, 163,
    354, 666, 933, 424, 341, 533, 870, 227, 730, 475, 186, 263, 647, 537, 686, 600, 224, 469, 68,
    770, 919, 190, 373, 294, 822, 808, 206, 184, 943, 795, 384, 383, 461, 404, 758, 839, 887, 715,
    67, 618, 276, 204, 918, 873, 777, 604, 560, 951, 160, 578, 722, 79, 804, 96, 409, 713, 940,
    652, 934, 970, 447, 318, 353, 859, 672, 112, 785, 645, 863, 803, 350, 139, 93, 354, 99, 820,
    908, 609, 772, 154, 274, 580, 184, 79, 626, 630, 742, 653, 282, 762, 623, 680, 81, 927, 626,
    789, 125, 411, 521, 938, 300, 821, 78, 343, 175, 128, 250, 170, 774, 972, 275, 999, 639, 495,
    78, 352, 126, 857, 956, 358, 619, 580, 124, 737, 594, 701, 612, 669, 112, 134, 694, 363, 992,
    809, 743, 168, 974, 944, 375, 748, 52, 600, 747, 642, 182, 862, 81, 344, 805, 988, 739, 511,
    655, 814, 334, 249, 515, 897, 955, 664, 981, 649, 113, 974, 459, 893, 228, 433, 837, 553, 268,
    926, 240, 102, 654, 459, 51, 686, 754, 806, 760, 493, 403, 415, 394, 687, 700, 946, 670, 656,
    610, 738, 392, 760, 799, 887, 653, 978, 321, 576, 617, 626, 502, 894, 679, 243, 440, 680, 879,
    194, 572, 640, 724, 926, 56, 204, 700, 707, 151, 457, 449, 797, 195, 791, 558, 945, 679, 297,
    59, 87, 824, 713, 663, 412, 693, 342, 606, 134, 108, 571, 364, 631, 212, 174, 643, 304, 329,
    343, 97, 430, 751, 497, 314, 983, 374, 822, 928, 140, 206, 73, 263, 980, 736, 876, 478, 430,
    305, 170, 514, 364, 692, 829, 82, 855, 953, 676, 246, 369, 970, 294, 750, 807, 827, 150, 790,
    288, 923, 804, 378, 215, 828, 592, 281, 565, 555, 710, 82, 896, 831, 547, 261, 524, 462, 293,
    465, 502, 56, 661, 821, 976, 991, 658, 869, 905, 758, 745, 193, 768, 550, 608, 933, 378, 286,
    215, 979, 792, 961, 61, 688, 793, 644, 986, 403, 106, 366, 905, 644, 372, 567, 466, 434, 645,
    210, 389, 550, 919, 135, 780, 773, 635, 389, 707, 100, 626, 958, 165, 504, 920, 176, 193, 713,
    857, 265, 203, 50, 668, 108, 645, 990, 626, 197, 510, 357, 358, 850, 858, 364, 936, 638,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    Truncated,
    Damaged(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Magic {
    Block,
    End,
}

pub(crate) fn bits48(bytes: &[u8], bit: u64) -> Option<u64> {
    let first = usize::try_from(bit / 8).ok()?;
    let shift = (bit % 8) as u32;
    let covered = if shift == 0 { 6 } else { 7 };
    let window = bytes.get(first..)?.get(..covered)?;
    let mut eight = [0u8; 8];
    eight[..covered].copy_from_slice(window);
    Some((u64::from_be_bytes(eight) << shift) >> 16)
}

pub(crate) fn magic_at(bytes: &[u8], bit: u64) -> Option<Magic> {
    match bits48(bytes, bit)? {
        BLOCK_MAGIC => Some(Magic::Block),
        END_MAGIC => Some(Magic::End),
        _ => None,
    }
}

const fn second_bytes() -> [bool; 256] {
    let mut marks = [false; 256];
    let mut shift = 0;
    while shift < 8 {
        marks[((BLOCK_MAGIC >> (32 + shift)) & 0xFF) as usize] = true;
        marks[((END_MAGIC >> (32 + shift)) & 0xFF) as usize] = true;
        shift += 1;
    }
    marks
}

const SECOND_BYTE: [bool; 256] = second_bytes();

pub(crate) fn next_magic(bytes: &[u8], from: u64) -> Option<(u64, Magic)> {
    let mut first = usize::try_from(from / 8).ok()?;
    let mut shift = from % 8;
    while let Some(&second) = bytes.get(first.checked_add(1)?) {
        if SECOND_BYTE[usize::from(second)] {
            while shift < 8 {
                let bit = first as u64 * 8 + shift;
                if let Some(magic) = magic_at(bytes, bit) {
                    return Some((bit, magic));
                }
                shift += 1;
            }
        }
        first += 1;
        shift = 0;
    }
    None
}

struct Bits<'a> {
    bytes: &'a [u8],
    next: usize,
    held: u64,
    live: u32,
}

impl<'a> Bits<'a> {
    fn at(bytes: &'a [u8], bit: u64) -> Self {
        let mut bits = Bits {
            bytes,
            next: usize::try_from(bit / 8).unwrap_or(usize::MAX),
            held: 0,
            live: 0,
        };
        bits.refill();
        bits.skip((bit % 8) as u32);
        bits
    }

    #[inline(always)]
    fn refill(&mut self) {
        let rest = self.bytes.get(self.next..).unwrap_or(&[]);
        let word = match rest.first_chunk::<8>() {
            Some(eight) => u64::from_be_bytes(*eight),
            None => {
                let mut eight = [0u8; 8];
                eight[..rest.len()].copy_from_slice(rest);
                u64::from_be_bytes(eight)
            }
        };
        self.held |= word >> self.live;
        self.next = self.next.saturating_add(((63 - self.live) / 8) as usize);
        self.live |= 56;
    }

    #[inline(always)]
    fn skip(&mut self, count: u32) {
        self.held <<= count;
        self.live -= count;
    }

    #[inline(always)]
    fn peek(&self, count: u32) -> u32 {
        (self.held >> (64 - count)) as u32
    }

    fn take(&mut self, count: u32) -> u32 {
        self.refill();
        let value = self.peek(count);
        self.skip(count);
        value
    }

    fn bit(&mut self) -> bool {
        self.take(1) == 1
    }

    fn position(&self) -> u64 {
        (self.next as u64).saturating_mul(8) - u64::from(self.live)
    }
}

struct Start {
    stored_crc: u32,
    randomised: bool,
    origin: u32,
}

fn block_start(bits: &mut Bits, level: u8) -> Result<Start, Refusal> {
    let magic = u64::from(bits.take(24)) << 24 | u64::from(bits.take(24));
    if magic != BLOCK_MAGIC {
        return Err(Refusal::Damaged(
            "bzip2 block does not start with its magic",
        ));
    }
    let stored_crc = bits.take(32);
    let randomised = bits.bit();
    let origin = bits.take(24);
    if origin as usize > 10 + BYTES_PER_LEVEL * usize::from(level) {
        return Err(Refusal::Damaged("bzip2 block starts past its largest size"));
    }
    Ok(Start {
        stored_crc,
        randomised,
        origin,
    })
}

fn used_bytes(bits: &mut Bits, used: &mut [u8; 256]) -> Result<usize, Refusal> {
    let ranges = bits.take(16);
    let mut count = 0;
    for range in 0..16u32 {
        if ranges & (0x8000 >> range) == 0 {
            continue;
        }
        let members = bits.take(16);
        for member in 0..16u32 {
            if members & (0x8000 >> member) != 0 {
                used[count] = (range * 16 + member) as u8;
                count += 1;
            }
        }
    }
    if count == 0 {
        return Err(Refusal::Damaged("bzip2 block uses no byte value"));
    }
    Ok(count)
}

fn selectors(bits: &mut Bits, tables: u8, kept: &mut Vec<u8>) -> Result<(), Refusal> {
    let count = bits.take(15) as usize;
    if count == 0 {
        return Err(Refusal::Damaged("bzip2 block has no selectors"));
    }
    kept.clear();
    let mut recent: [u8; 6] = [0, 1, 2, 3, 4, 5];
    for index in 0..count {
        let mut rank = 0u8;
        while bits.bit() {
            rank += 1;
            if rank >= tables {
                return Err(Refusal::Damaged("bzip2 selector names a missing table"));
            }
        }
        // EXTERNAL(libbzip2): since 1.0.8 it reads selectors past 18,002 and drops them.
        if index < KEPT_SELECTORS {
            let table = recent[usize::from(rank)];
            recent.copy_within(0..usize::from(rank), 1);
            recent[0] = table;
            kept.push(table);
        }
    }
    Ok(())
}

fn code_lengths(bits: &mut Bits, lengths: &mut [u8]) -> Result<(), Refusal> {
    let mut length = bits.take(5);
    for slot in lengths {
        loop {
            if !(1..=LONGEST_CODE).contains(&length) {
                return Err(Refusal::Damaged("bzip2 code length is outside 1 to 20"));
            }
            if !bits.bit() {
                break;
            }
            if bits.bit() {
                length -= 1;
            } else {
                length += 1;
            }
        }
        *slot = length as u8;
    }
    Ok(())
}

struct Table {
    limit: [i32; 23],
    base: [i32; 23],
    order: [u16; LARGEST_ALPHABET],
    shortest: u32,
    lookup: [u16; 1 << LOOKUP_BITS],
}

impl Table {
    // EXTERNAL(libbzip2): hbCreateDecodeTables, kept step for step so a code that is not a full
    // prefix code decodes or fails exactly as it does there.
    fn new(lengths: &[u8]) -> Table {
        let mut table = Table {
            limit: [0; 23],
            base: [0; 23],
            order: [0; LARGEST_ALPHABET],
            shortest: 32,
            lookup: [SLOW; 1 << LOOKUP_BITS],
        };
        let mut longest = 0;
        for &length in lengths {
            table.shortest = table.shortest.min(u32::from(length));
            longest = longest.max(u32::from(length));
        }
        let (shortest, longest) = (table.shortest as usize, longest as usize);
        let mut filled = 0;
        for length in shortest..=longest {
            for (symbol, &its) in lengths.iter().enumerate() {
                if usize::from(its) == length {
                    table.order[filled] = symbol as u16;
                    filled += 1;
                }
            }
        }
        for &length in lengths {
            table.base[usize::from(length) + 1] += 1;
        }
        for length in 1..23 {
            table.base[length] += table.base[length - 1];
        }
        let mut code = 0;
        for length in shortest..=longest {
            code += table.base[length + 1] - table.base[length];
            table.limit[length] = code - 1;
            code <<= 1;
        }
        for length in shortest..longest {
            table.base[length + 1] = 2 * (table.limit[length] + 1) - table.base[length + 1];
        }
        for prefix in 0..1u32 << LOOKUP_BITS {
            let mut length = table.shortest;
            while length <= LOOKUP_BITS {
                if let Some(symbol) = table.symbol(prefix >> (LOOKUP_BITS - length), length) {
                    table.lookup[prefix as usize] =
                        symbol.map_or(SLOW, |symbol| symbol << 5 | length as u16);
                    break;
                }
                length += 1;
            }
        }
        table
    }

    #[inline(always)]
    fn symbol(&self, code: u32, length: u32) -> Option<Option<u16>> {
        let code = code as i32;
        if code > self.limit[length as usize] {
            return None;
        }
        let index = code - self.base[length as usize];
        Some(
            usize::try_from(index)
                .ok()
                .and_then(|index| self.order.get(index))
                .copied(),
        )
    }

    #[cold]
    fn slow(&self, bits: &mut Bits) -> Result<u16, Refusal> {
        let mut length = self.shortest;
        while length <= LONGEST_CODE {
            if let Some(symbol) = self.symbol(bits.peek(length), length) {
                bits.skip(length);
                return symbol.ok_or(Refusal::Damaged("bzip2 code names no symbol"));
            }
            length += 1;
        }
        Err(Refusal::Damaged("bzip2 code is longer than 20 bits"))
    }
}

#[inline(always)]
fn to_front(order: &mut [u8; 256], index: usize) -> u8 {
    let byte = order[index];
    let (whole, last) = order.split_at_mut(index / 16 * 16);
    let mut carried = u128::from(byte);
    for word in whole.chunks_exact_mut(16) {
        let Ok(sixteen) = <&mut [u8; 16]>::try_from(word) else {
            continue;
        };
        let list = u128::from_le_bytes(*sixteen);
        *sixteen = (list << 8 | carried).to_le_bytes();
        carried = list >> 120;
    }
    if let Some(sixteen) = last.first_chunk_mut::<16>() {
        let list = u128::from_le_bytes(*sixteen);
        let below = (1u128 << (8 * (index % 16))) - 1;
        let moved = (list & !((below << 8) | 0xFF)) | ((list & below) << 8) | carried;
        *sixteen = moved.to_le_bytes();
    }
    byte
}

struct Column<'a> {
    bytes: &'a mut [u8],
    filled: usize,
    counts: [u32; 256],
}

fn last_column(
    bits: &mut Bits,
    limit: u64,
    tables: &[Table],
    selectors: &[u8],
    used: &[u8],
    column: &mut Column,
) -> Result<(), Refusal> {
    let end_of_block = used.len() as u16 + 1;
    let mut order = [0u8; 256];
    order[..used.len()].copy_from_slice(used);
    let (mut run, mut run_bit) = (0usize, 0u32);
    for &selector in selectors {
        if bits.position() > limit {
            return Err(Refusal::Truncated);
        }
        let table = &tables[usize::from(selector)];
        for _ in 0..SYMBOLS_PER_SELECTOR {
            bits.refill();
            let entry = table.lookup[bits.peek(LOOKUP_BITS) as usize];
            let symbol = if entry == SLOW {
                table.slow(bits)?
            } else {
                bits.skip(u32::from(entry & 31));
                entry >> 5
            };
            if symbol <= 1 {
                if run_bit >= LARGEST_RUN_BIT {
                    return Err(Refusal::Damaged("bzip2 run is longer than any block"));
                }
                run += (usize::from(symbol) + 1) << run_bit;
                run_bit += 1;
                continue;
            }
            if run_bit > 0 {
                let byte = order[0];
                let Some(repeated) = column.bytes.get_mut(column.filled..column.filled + run)
                else {
                    return Err(Refusal::Damaged(
                        "bzip2 block decodes past its largest size",
                    ));
                };
                repeated.fill(byte);
                column.filled += run;
                column.counts[usize::from(byte)] += run as u32;
                (run, run_bit) = (0, 0);
            }
            if symbol == end_of_block {
                return Ok(());
            }
            let Some(slot) = column.bytes.get_mut(column.filled) else {
                return Err(Refusal::Damaged(
                    "bzip2 block decodes past its largest size",
                ));
            };
            let byte = to_front(&mut order, usize::from(symbol) - 1);
            *slot = byte;
            column.filled += 1;
            column.counts[usize::from(byte)] += 1;
        }
    }
    Err(Refusal::Damaged(
        "bzip2 block has more symbols than selectors",
    ))
}

fn first_column_starts(counts: &[u32; 256]) -> [u32; 256] {
    let mut starts = [0u32; 256];
    let mut total = 0;
    for (start, &count) in starts.iter_mut().zip(counts) {
        *start = total;
        total += count;
    }
    starts
}

fn text_before_runs(column: &[u8], counts: &[u32; 256], origin: u32) -> Vec<u8> {
    let mut starts = first_column_starts(counts);
    let mut onward = vec![0u32; column.len()];
    for (row, &byte) in column.iter().enumerate() {
        let start = &mut starts[usize::from(byte)];
        onward[*start as usize] = (row as u32) << 8 | u32::from(byte);
        *start += 1;
    }
    let mut text = vec![0u8; column.len()];
    let mut row = origin;
    for byte in &mut text {
        let link = onward[row as usize];
        *byte = link as u8;
        row = link >> 8;
    }
    text
}

fn text_before_runs_walked_from_both_ends(
    column: &[u8],
    counts: &[u32; 256],
    origin: u32,
) -> Vec<u8> {
    let mut starts = first_column_starts(counts);
    let mut onward = vec![0u32; column.len()];
    let mut back = vec![0u32; column.len()];
    for ((row, &byte), back) in column.iter().enumerate().zip(&mut back) {
        let start = &mut starts[usize::from(byte)];
        onward[*start as usize] = (row as u32) << 8 | u32::from(byte);
        *back = *start << 8 | u32::from(byte);
        *start += 1;
    }
    let mut text = vec![0u8; column.len()];
    let (first_half, second_half) = text.split_at_mut(column.len().div_ceil(2));
    let (mut row, mut back_row) = (origin, origin);
    for (byte, mirrored) in first_half.iter_mut().zip(second_half.iter_mut().rev()) {
        let (link, back_link) = (onward[row as usize], back[back_row as usize]);
        (*byte, *mirrored) = (link as u8, back_link as u8);
        (row, back_row) = (link >> 8, back_link >> 8);
    }
    if first_half.len() > second_half.len() {
        let link = onward[row as usize];
        first_half[second_half.len()] = link as u8;
        row = link >> 8;
    }
    if row != back_row {
        for byte in second_half {
            let link = onward[row as usize];
            *byte = link as u8;
            row = link >> 8;
        }
    }
    text
}

fn undo_randomising(text: &mut [u8]) {
    let (mut step, mut left) = (0, 0u16);
    for byte in text {
        if left == 0 {
            left = RANDOM_STEPS[step];
            step = (step + 1) % RANDOM_STEPS.len();
        }
        left -= 1;
        *byte ^= u8::from(left == 1);
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Walk {
    Onward,
    FromBothEnds,
}

pub(crate) struct Block {
    pub(crate) text: Vec<u8>,
    pub(crate) stored_crc: u32,
    pub(crate) end: u64,
}

pub(crate) fn decode_block(
    bytes: &[u8],
    start: u64,
    limit: u64,
    level: u8,
    walk: Walk,
) -> Result<Block, Refusal> {
    let mut bits = Bits::at(bytes, start);
    let mut column = vec![0u8; BYTES_PER_LEVEL * usize::from(level)];
    let mut column = Column {
        bytes: &mut column,
        filled: 0,
        counts: [0; 256],
    };
    let decoded = symbols(&mut bits, limit, level, &mut column);
    if bits.position() > limit {
        return Err(Refusal::Truncated);
    }
    let start = decoded?;
    if start.origin as usize >= column.filled {
        return Err(Refusal::Damaged("bzip2 block starts past its end"));
    }
    let column_read = &column.bytes[..column.filled];
    let mut text = match walk {
        Walk::Onward => text_before_runs(column_read, &column.counts, start.origin),
        Walk::FromBothEnds => {
            text_before_runs_walked_from_both_ends(column_read, &column.counts, start.origin)
        }
    };
    if start.randomised {
        undo_randomising(&mut text);
    }
    Ok(Block {
        text,
        stored_crc: start.stored_crc,
        end: bits.position(),
    })
}

fn symbols(bits: &mut Bits, limit: u64, level: u8, column: &mut Column) -> Result<Start, Refusal> {
    let start = block_start(bits, level)?;
    let mut used = [0u8; 256];
    let used_count = used_bytes(bits, &mut used)?;
    let table_count = bits.take(3) as u8;
    if !(2..=6).contains(&table_count) {
        return Err(Refusal::Damaged("bzip2 block needs 2 to 6 code tables"));
    }
    let mut kept = Vec::new();
    selectors(bits, table_count, &mut kept)?;
    let mut tables = Vec::with_capacity(usize::from(table_count));
    for _ in 0..table_count {
        let mut lengths = [0u8; LARGEST_ALPHABET];
        let lengths = &mut lengths[..used_count + 2];
        code_lengths(bits, lengths)?;
        if bits.position() > limit {
            return Err(Refusal::Truncated);
        }
        tables.push(Table::new(lengths));
    }
    last_column(bits, limit, &tables, &kept, &used[..used_count], column)?;
    Ok(start)
}

pub(crate) struct Runs {
    text: Vec<u8>,
    at: usize,
    literal_end: usize,
    length_follows: bool,
    byte: u8,
    repeats: usize,
}

fn first_run_of_four(bytes: &[u8]) -> Option<usize> {
    let mut start = 0;
    while let Some(window) = bytes.get(start..start + SCAN_STEP + 3) {
        let mut any = false;
        for at in 0..SCAN_STEP {
            any |= (window[at] == window[at + 1])
                & (window[at + 1] == window[at + 2])
                & (window[at + 2] == window[at + 3]);
        }
        if any {
            break;
        }
        start += SCAN_STEP;
    }
    bytes[start..]
        .windows(4)
        .position(|four| four[0] == four[1] && four[1] == four[2] && four[2] == four[3])
        .map(|found| start + found)
}

impl Runs {
    pub(crate) fn new(text: Vec<u8>) -> Self {
        Runs {
            text,
            at: 0,
            literal_end: 0,
            length_follows: false,
            byte: 0,
            repeats: 0,
        }
    }

    pub(crate) fn read(&mut self, output: &mut [u8]) -> Result<usize, Refusal> {
        let mut written = 0;
        loop {
            let repeated = self.repeats.min(output.len() - written);
            output[written..written + repeated].fill(self.byte);
            self.repeats -= repeated;
            written += repeated;
            let literal = (self.literal_end - self.at).min(output.len() - written);
            output[written..written + literal]
                .copy_from_slice(&self.text[self.at..self.at + literal]);
            self.at += literal;
            written += literal;
            if written == output.len() {
                return Ok(written);
            }
            if self.length_follows {
                let Some(&length) = self.text.get(self.at) else {
                    return Err(Refusal::Damaged("bzip2 block ends inside a run"));
                };
                self.byte = self.text[self.at - 1];
                self.repeats = usize::from(length);
                self.at += 1;
                self.literal_end = self.at;
                self.length_follows = false;
                continue;
            }
            if self.at == self.text.len() {
                return Ok(written);
            }
            match first_run_of_four(&self.text[self.at..]) {
                Some(run) => {
                    self.literal_end = self.at + run + 4;
                    self.length_follows = true;
                }
                None => self.literal_end = self.text.len(),
            }
        }
    }
}

pub(crate) struct MsbFirstCrc32(crc32fast::Hasher);

fn with_each_byte_bit_reversed(bytes: &[u8], reversed: &mut [u8]) {
    for (to, from) in reversed.iter_mut().zip(bytes) {
        *to = from.reverse_bits();
    }
}

impl MsbFirstCrc32 {
    pub(crate) fn new() -> Self {
        MsbFirstCrc32(crc32fast::Hasher::new())
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        let mut reversed = [0u8; 4096];
        for piece in bytes.chunks(reversed.len()) {
            with_each_byte_bit_reversed(piece, &mut reversed);
            self.0.update(&reversed[..piece.len()]);
        }
    }

    pub(crate) fn value(self) -> u32 {
        self.0.finalize().reverse_bits()
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    fn bit_of(bytes: &[u8], bit: u64) -> u64 {
        match bytes.get((bit / 8) as usize) {
            Some(byte) => u64::from(byte >> (7 - bit % 8) & 1),
            None => 0,
        }
    }

    #[kani::proof]
    #[kani::unwind(9)]
    fn the_scanner_returns_the_first_magic_and_never_panics() {
        let bytes: [u8; 7] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let bytes = &bytes[..length];
        let from: u64 = kani::any();
        let found = next_magic(bytes, from);
        let probe: u64 = kani::any();
        kani::assume(probe >= from && probe < 16);
        match found {
            Some((bit, magic)) => {
                assert!(bit >= from && bit + MAGIC_BITS <= length as u64 * 8);
                assert_eq!(magic_at(bytes, bit), Some(magic));
                assert!(probe >= bit || magic_at(bytes, probe).is_none());
            }
            None => assert!(magic_at(bytes, probe).is_none()),
        }
    }

    #[kani::proof]
    #[kani::unwind(49)]
    fn magics_are_read_only_from_48_whole_bits() {
        let bytes: [u8; 8] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let bit: u64 = kani::any();
        match bits48(&bytes[..length], bit) {
            Some(value) => {
                assert!(bit + MAGIC_BITS <= length as u64 * 8);
                let mut expected = 0;
                for offset in 0..MAGIC_BITS {
                    expected = expected << 1 | bit_of(&bytes, bit + offset);
                }
                assert_eq!(value, expected);
            }
            None => assert!(bit.saturating_add(MAGIC_BITS) > length as u64 * 8),
        }
    }

    #[kani::proof]
    #[kani::unwind(33)]
    fn bits_come_in_stream_order_with_zeros_past_the_end() {
        let bytes: [u8; 11] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let bytes = &bytes[..length];
        let start: u64 = kani::any();
        kani::assume(start <= 24);
        let (first, second): (u32, u32) = (kani::any(), kani::any());
        kani::assume((1..=32).contains(&first) && (1..=32).contains(&second));
        let mut bits = Bits::at(bytes, start);
        assert_eq!(bits.position(), start);
        let mut at = start;
        for count in [first, second] {
            let mut expected = 0;
            for offset in 0..u64::from(count) {
                expected = expected << 1 | bit_of(bytes, at + offset);
            }
            assert_eq!(u64::from(bits.take(count)), expected);
            at += u64::from(count);
            assert_eq!(bits.position(), at);
        }
    }

    #[kani::proof]
    #[kani::unwind(17)]
    fn block_starts_never_panic_and_start_inside_the_largest_block() {
        let bytes: [u8; 15] = kani::any();
        let start: u64 = kani::any();
        kani::assume(start < 8);
        let level: u8 = kani::any();
        kani::assume((1..=9).contains(&level));
        let mut bits = Bits::at(&bytes, start);
        if let Ok(block) = block_start(&mut bits, level) {
            assert!(block.origin as usize <= 10 + BYTES_PER_LEVEL * usize::from(level));
            assert_eq!(bits.position(), start + 105);
            assert_eq!(bits48(&bytes, start), Some(BLOCK_MAGIC));
        }
    }

    #[kani::proof]
    #[kani::unwind(17)]
    fn used_bytes_of_one_range_never_panic_and_come_out_in_order() {
        let bytes: [u8; 4] = kani::any();
        let mut bits = Bits::at(&bytes, 0);
        let mut used = [0u8; 256];
        let ranges = u16::from_be_bytes([bytes[0], bytes[1]]);
        let members = u16::from_be_bytes([bytes[2], bytes[3]]);
        kani::assume(ranges.count_ones() <= 1);
        match used_bytes(&mut bits, &mut used) {
            Ok(count) => {
                assert_eq!(count as u32, members.count_ones());
                assert_eq!(bits.position(), 32);
                let probe: usize = kani::any();
                kani::assume(probe < count - 1);
                assert!(used[probe] < used[probe + 1]);
                assert_eq!(u32::from(used[probe] / 16), ranges.leading_zeros());
            }
            Err(refusal) => {
                assert!(ranges == 0 || members == 0);
                assert!(matches!(refusal, Refusal::Damaged(_)));
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn selectors_never_panic_and_name_only_tables_the_block_has() {
        let bytes: [u8; 5] = kani::any();
        let tables: u8 = kani::any();
        kani::assume((2..=6).contains(&tables));
        let count = usize::from(u16::from_be_bytes([bytes[0], bytes[1]]) >> 1);
        kani::assume(count <= 4);
        let mut bits = Bits::at(&bytes, 0);
        let mut kept = Vec::with_capacity(4);
        if selectors(&mut bits, tables, &mut kept).is_ok() {
            assert_eq!(kept.len(), count);
            let probe: usize = kani::any();
            kani::assume(probe < kept.len());
            assert!(kept[probe] < tables);
        }
    }

    #[kani::proof]
    #[kani::unwind(22)]
    fn code_lengths_never_panic_and_stay_from_1_to_20() {
        let bytes: [u8; 5] = kani::any();
        let mut bits = Bits::at(&bytes, 0);
        let mut lengths = [0u8; 2];
        if code_lengths(&mut bits, &mut lengths).is_ok() {
            assert!((1..=20).contains(&lengths[0]) && (1..=20).contains(&lengths[1]));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};

    pub(crate) struct Noise(pub(crate) u64);

    impl Noise {
        pub(crate) fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        pub(crate) fn below(&mut self, limit: usize) -> usize {
            (self.next() % limit as u64) as usize
        }
    }

    pub(crate) fn sample(kind: usize, length: usize, noise: &mut Noise) -> Vec<u8> {
        let mut data = Vec::with_capacity(length);
        while data.len() < length {
            match kind {
                0 => data.push(noise.next() as u8),
                1 => {
                    let word =
                        [b"the ".as_slice(), b"quick ", b"brown ", b"fox ", b"\n"][noise.below(5)];
                    data.extend_from_slice(word);
                }
                2 => {
                    let run = [1, 2, 3, 4, 5, 6, 255, 256, 259, 260, 1000][noise.below(11)];
                    let byte = noise.below(3) as u8;
                    data.extend(std::iter::repeat_n(byte, run));
                }
                3 => data.push(0),
                4 => data.push(data.len() as u8),
                _ => data.push([4u8, 251, 255][noise.below(3)]),
            }
        }
        data.truncate(length);
        data
    }

    pub(crate) fn compress(data: &[u8], level: u32) -> Vec<u8> {
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::new(level));
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    pub(crate) fn theirs(stream: &[u8]) -> Option<Vec<u8>> {
        let mut decoded = Vec::new();
        bzip2::read::BzDecoder::new(stream)
            .read_to_end(&mut decoded)
            .ok()
            .map(|_| decoded)
    }

    pub(crate) fn magic_bits(magic: u64) -> Vec<bool> {
        (0..48).map(|bit| magic >> (47 - bit) & 1 == 1).collect()
    }

    fn to_bits(bytes: &[u8]) -> Vec<bool> {
        (0..bytes.len() * 8)
            .map(|bit| bytes[bit / 8] & (0x80 >> (bit % 8)) != 0)
            .collect()
    }

    fn to_bytes(bits: &[bool]) -> Vec<u8> {
        let mut bytes = vec![0u8; bits.len().div_ceil(8)];
        for (bit, _) in bits.iter().enumerate().filter(|(_, set)| **set) {
            bytes[bit / 8] |= 0x80 >> (bit % 8);
        }
        bytes
    }

    pub(crate) enum Spare {
        Selectors,
        SelectorsFromAWholeByte,
        LengthSteps,
    }

    pub(crate) fn with_spare_bits(
        stream: &[u8],
        block: usize,
        spare: Spare,
        held: &[bool],
    ) -> Vec<u8> {
        let level = stream[3] - b'0';
        let limit = stream.len() as u64 * 8;
        let mut at = 32;
        for _ in 0..block {
            at = decode_block(stream, at, limit, level, Walk::Onward)
                .ok()
                .unwrap()
                .end;
        }
        let mut bits = Bits::at(stream, at);
        block_start(&mut bits, level).ok().unwrap();
        used_bytes(&mut bits, &mut [0u8; 256]).unwrap();
        let tables = bits.take(3) as u8;
        let count_at = bits.position() as usize;
        let mut kept = Vec::new();
        selectors(&mut bits, tables, &mut kept).unwrap();
        let selectors_end = bits.position() as usize;
        let mut end = at;
        while magic_at(stream, end) == Some(Magic::Block) {
            end = decode_block(stream, end, limit, level, Walk::Onward)
                .ok()
                .unwrap()
                .end;
        }
        let old = to_bits(stream);
        let mut new = old[..count_at].to_vec();
        match spare {
            Spare::Selectors | Spare::SelectorsFromAWholeByte => {
                let unaligned = match spare {
                    Spare::SelectorsFromAWholeByte => selectors_end % 8,
                    _ => 0,
                };
                let mut held = [&vec![false; (8 - unaligned) % 8], held].concat();
                if held.last() == Some(&true) {
                    held.push(false);
                }
                let longest_rank = held.split(|bit| !bit).map(|ones| ones.len()).max().unwrap();
                assert!(longest_rank < usize::from(tables));
                let count = kept.len() + held.iter().filter(|bit| !**bit).count();
                assert!(count <= KEPT_SELECTORS);
                new.extend((0..15).map(|bit| count >> (14 - bit) & 1 == 1));
                new.extend_from_slice(&old[count_at + 15..selectors_end]);
                new.extend_from_slice(&held);
                new.extend_from_slice(&old[selectors_end..end as usize + 80]);
            }
            Spare::LengthSteps => {
                assert!(bits.take(5) < 20);
                assert!(held.chunks(4).all(|step| step == [true, false, true, true]));
                new.extend_from_slice(&old[count_at..selectors_end + 5]);
                new.extend_from_slice(held);
                new.extend_from_slice(&old[selectors_end + 5..end as usize + 80]);
            }
        }
        to_bytes(&new)
    }

    pub(crate) fn with_bits_after_block(stream: &[u8], block: usize, extra: &[bool]) -> Vec<u8> {
        let (level, limit) = (stream[3] - b'0', stream.len() as u64 * 8);
        let mut end = 32;
        for _ in 0..=block {
            end = decode_block(stream, end, limit, level, Walk::Onward)
                .ok()
                .unwrap()
                .end;
        }
        let mut bits = to_bits(stream);
        bits.splice(end as usize..end as usize, extra.iter().copied());
        to_bytes(&bits)
    }

    fn ours(stream: &[u8]) -> Result<Vec<u8>, Refusal> {
        let damaged = Refusal::Damaged("test stream");
        let level = match stream {
            [b'B', b'Z', b'h', level @ b'1'..=b'9', ..] => level - b'0',
            _ => return Err(damaged),
        };
        let (mut at, mut combined, mut decoded) = (32, 0u32, Vec::new());
        let limit = stream.len() as u64 * 8;
        loop {
            match magic_at(stream, at) {
                Some(Magic::Block) => {
                    let block = decode_block(stream, at, limit, level, Walk::Onward)?;
                    let both = decode_block(stream, at, limit, level, Walk::FromBothEnds)?;
                    assert!(both.text == block.text && both.end == block.end);
                    let (mut runs, mut crc) = (Runs::new(block.text), MsbFirstCrc32::new());
                    let mut piece = [0u8; 1000];
                    loop {
                        let read = runs.read(&mut piece)?;
                        if read == 0 {
                            break;
                        }
                        crc.update(&piece[..read]);
                        decoded.extend_from_slice(&piece[..read]);
                    }
                    let crc = crc.value();
                    if crc != block.stored_crc {
                        return Err(damaged);
                    }
                    combined = combined.rotate_left(1) ^ crc;
                    at = block.end;
                }
                Some(Magic::End) => {
                    let stored = bits48(stream, at + 32).ok_or(Refusal::Truncated)? as u32;
                    return if stored == combined {
                        Ok(decoded)
                    } else {
                        Err(damaged)
                    };
                }
                None => return Err(damaged),
            }
        }
    }

    fn bitwise_crc(bytes: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for &byte in bytes {
            crc ^= u32::from(byte) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 {
                    crc << 1 ^ 0x04C1_1DB7
                } else {
                    crc << 1
                };
            }
        }
        !crc
    }

    #[test]
    fn the_crc_is_the_bitwise_bzip2_crc_on_every_split() {
        let mut crc = MsbFirstCrc32::new();
        crc.update(b"123456789");
        assert_eq!(crc.value(), 0xFC89_1918);
        let mut noise = Noise(7);
        let data = sample(0, 9000, &mut noise);
        for length in (0..300).chain([4095, 4096, 4097, 9000]) {
            for split in [0, 1, length / 2, length] {
                let mut crc = MsbFirstCrc32::new();
                crc.update(&data[..split.min(length)]);
                crc.update(&data[split.min(length)..length]);
                assert_eq!(
                    crc.value(),
                    bitwise_crc(&data[..length]),
                    "{length} {split}"
                );
            }
        }
    }

    #[test]
    fn the_scanner_finds_the_first_magic_a_bit_at_a_time_search_finds() {
        let mut noise = Noise(11);
        for round in 0..4000 {
            let mut bytes = sample(0, 4 + noise.below(40), &mut noise);
            for _ in 0..noise.below(4) {
                let magic = if noise.below(2) == 0 {
                    BLOCK_MAGIC
                } else {
                    END_MAGIC
                };
                let at = noise.below(bytes.len() * 8);
                for bit in 0..48 {
                    let (byte, mask) = ((at + bit) / 8, 0x80 >> ((at + bit) % 8));
                    if let Some(byte) = bytes.get_mut(byte) {
                        *byte = if magic >> (47 - bit) & 1 == 1 {
                            *byte | mask
                        } else {
                            *byte & !mask
                        };
                    }
                }
            }
            let from = noise.below(bytes.len() * 8 + 9) as u64;
            let slow = (from..bytes.len() as u64 * 8)
                .find_map(|bit| magic_at(&bytes, bit).map(|magic| (bit, magic)));
            assert_eq!(next_magic(&bytes, from), slow, "round {round}");
        }
    }

    #[test]
    fn every_kind_of_data_at_every_level_decodes_as_the_bzip2_crate_does() {
        let mut noise = Noise(3);
        for kind in 0..6 {
            for level in 1..=9 {
                for length in [0, 1, 4, 5, 300, 99_999, 100_001, 250_000 * level as usize] {
                    let data = sample(kind, length, &mut noise);
                    let stream = compress(&data, level);
                    assert_eq!(theirs(&stream).as_ref(), Some(&data));
                    assert!(
                        ours(&stream).as_ref() == Ok(&data),
                        "{kind} {level} {length}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_cut_and_many_bit_flips_decode_or_fail_as_the_bzip2_crate_does() {
        let mut noise = Noise(5);
        for (kind, length, level) in [(1, 700, 1), (2, 3000, 1), (0, 300, 9), (5, 250_000, 1)] {
            let data = sample(kind, length, &mut noise);
            let stream = compress(&data, level);
            for cut in (0..stream.len()).rev().take(600) {
                assert_eq!(theirs(&stream[..cut]), None);
                assert!(ours(&stream[..cut]).is_err(), "{kind} cut {cut}");
            }
            for _ in 0..3000 {
                let mut flipped = stream.clone();
                let bit = noise.below(stream.len() * 8);
                flipped[bit / 8] ^= 0x80 >> (bit % 8);
                assert_eq!(ours(&flipped).ok(), theirs(&flipped), "{kind} bit {bit}");
            }
            for bit in 0..(stream.len() * 8).min(2000) {
                let mut flipped = stream.clone();
                flipped[bit / 8] ^= 0x80 >> (bit % 8);
                assert_eq!(ours(&flipped).ok(), theirs(&flipped), "{kind} bit {bit}");
            }
        }
    }

    #[test]
    fn a_magic_inside_a_block_does_not_end_it() {
        let mut noise = Noise(13);
        let data = sample(0, 250_000, &mut noise);
        let stream = compress(&data, 1);
        for magic in [BLOCK_MAGIC, END_MAGIC] {
            for block in 0..3 {
                let planted = with_spare_bits(&stream, block, Spare::Selectors, &magic_bits(magic));
                let found = (33..planted.len() as u64 * 8)
                    .filter(|&bit| magic_at(&planted, bit).is_some())
                    .count();
                assert_eq!(found, 4, "three blocks, the end and the planted magic");
                assert_eq!(theirs(&planted).as_ref(), Some(&data));
                assert!(ours(&planted).as_ref() == Ok(&data), "{magic:x} {block}");
            }
        }
    }

    #[test]
    fn the_lookup_decodes_every_code_as_the_bit_at_a_time_walk_does() {
        let mut noise = Noise(19);
        for round in 0..3000 {
            let longest = [3, 9, 10, 11, 20][round % 5];
            let lengths: Vec<u8> = (0..3 + noise.below(40))
                .map(|_| 1 + noise.below(longest) as u8)
                .collect();
            let table = Table::new(&lengths);
            for _ in 0..200 {
                let bytes = (noise.next() >> 8).to_be_bytes();
                let mut walked = Bits::at(&bytes, 0);
                walked.refill();
                let slow = table.slow(&mut walked).ok();
                let mut looked = Bits::at(&bytes, 0);
                looked.refill();
                let entry = table.lookup[looked.peek(LOOKUP_BITS) as usize];
                if entry == SLOW {
                    assert!(slow.is_none() || walked.position() > u64::from(LOOKUP_BITS));
                } else {
                    looked.skip(u32::from(entry & 31));
                    assert_eq!(Some(entry >> 5), slow, "{lengths:?}");
                    assert_eq!(looked.position(), walked.position(), "{lengths:?}");
                }
                if let Some(symbol) = slow {
                    assert!(usize::from(symbol) < lengths.len());
                }
            }
        }
    }

    #[test]
    fn used_bytes_are_the_set_bits_of_the_two_level_map() {
        let mut noise = Noise(18);
        for _ in 0..20_000 {
            let ranges = (noise.next() & noise.next()) as u16;
            let mut bytes = ranges.to_be_bytes().to_vec();
            let mut expected = Vec::new();
            for range in (0..16).filter(|range| ranges & (0x8000 >> range) != 0) {
                let members = noise.next() as u16;
                bytes.extend(members.to_be_bytes());
                expected.extend(
                    (0..16)
                        .filter(|member| members & (0x8000 >> member) != 0)
                        .map(|member| (range * 16 + member) as u8),
                );
            }
            let (mut bits, mut used) = (Bits::at(&bytes, 0), [0u8; 256]);
            match used_bytes(&mut bits, &mut used) {
                Ok(count) => assert_eq!(&used[..count], &expected[..]),
                Err(_) => assert!(expected.is_empty()),
            }
            assert_eq!(bits.position(), bytes.len() as u64 * 8);
        }
    }

    #[test]
    fn randomised_blocks_decode_as_the_bzip2_crate_does() {
        let mut noise = Noise(29);
        let mut compared = 0;
        for round in 0..12 {
            let data = sample(round % 3, 1500 + 700 * round, &mut noise);
            let mut bits = to_bits(&compress(&data, 1));
            bits[32 + 48 + 32] = true;
            let randomised = to_bytes(&bits);
            let mut theirs_before_the_crc = vec![0u8; 100_000];
            let mut raw = bzip2::Decompress::new(false);
            loop {
                let (taken, given) = (raw.total_in() as usize, raw.total_out() as usize);
                let step =
                    raw.decompress(&randomised[taken..], &mut theirs_before_the_crc[given..]);
                let stalled = raw.total_in() as usize == taken && raw.total_out() as usize == given;
                if step.is_err() || stalled {
                    break;
                }
            }
            theirs_before_the_crc.truncate(raw.total_out() as usize);
            let crc = bitwise_crc(&theirs_before_the_crc);
            let end = decode_block(&randomised, 32, bits.len() as u64, 1, Walk::Onward)
                .ok()
                .unwrap()
                .end;
            for field in [80, end as usize + 48] {
                for bit in 0..32 {
                    bits[field + bit] = crc >> (31 - bit) & 1 == 1;
                }
            }
            let patched = to_bytes(&bits);
            if let Some(expected) = theirs(&patched) {
                assert!(expected == theirs_before_the_crc, "round {round}");
                assert!(ours(&patched).as_ref() == Ok(&expected), "round {round}");
                compared += usize::from(expected != data);
            } else {
                assert!(ours(&patched).is_err(), "round {round}");
            }
        }
        assert!(compared >= 4, "{compared}");
    }

    #[test]
    fn moving_to_the_front_matches_a_rotation_at_every_index() {
        let mut noise = Noise(17);
        let mut order: [u8; 256] = std::array::from_fn(|index| index as u8);
        for round in 0..20_000 {
            let index = if round < 256 { round } else { noise.below(256) };
            let mut rotated = order;
            rotated[..=index].rotate_right(1);
            assert_eq!(to_front(&mut order, index), rotated[0]);
            assert_eq!(order, rotated, "{index}");
        }
    }

    #[test]
    fn a_run_of_four_takes_the_next_byte_as_its_length_and_must_have_one() {
        let expanded = |text: &[u8]| {
            let (mut runs, mut output) = (Runs::new(text.to_vec()), [0u8; 600]);
            runs.read(&mut output).map(|read| output[..read].to_vec())
        };
        assert_eq!(expanded(&[7, 7, 7, 7, 2]), Ok(vec![7; 6]));
        assert_eq!(expanded(&[7, 7, 7, 7, 0, 7]), Ok(vec![7; 5]));
        assert_eq!(
            expanded(&[1, 7, 7, 7, 7, 255, 7, 7, 7]).map(|all| all.len()),
            Ok(263)
        );
        assert_eq!(
            expanded(&[7, 7, 7, 7, 4, 4, 4, 4]),
            Ok(vec![7, 7, 7, 7, 7, 7, 7, 7, 4, 4, 4])
        );
        assert!(expanded(&[7, 7, 7, 7]).is_err());
        assert!(expanded(&[1, 2, 7, 7, 7, 7]).is_err());
        assert_eq!(expanded(&[7, 7, 7]), Ok(vec![7; 3]));
    }

    #[test]
    fn runs_expand_the_same_into_every_size_of_read() {
        let mut noise = Noise(9);
        for kind in [2, 3, 5] {
            let text = sample(kind, 3000, &mut noise);
            let whole = {
                let (mut runs, mut whole) = (Runs::new(text.clone()), vec![0u8; 1 << 20]);
                let read = runs.read(&mut whole);
                read.map(|read| whole[..read].to_vec())
            };
            for size in [1, 2, 3, 4, 5, 7, 31, 32, 33, 255, 256, 257, 1000] {
                let (mut runs, mut pieces, mut piece) =
                    (Runs::new(text.clone()), Vec::new(), vec![0u8; size]);
                let outcome = loop {
                    match runs.read(&mut piece) {
                        Ok(0) => break Ok(()),
                        Ok(read) => pieces.extend_from_slice(&piece[..read]),
                        Err(refusal) => break Err(refusal),
                    }
                };
                match &whole {
                    Ok(whole) => assert!(outcome.is_ok() && &pieces == whole, "{kind} {size}"),
                    Err(_) => assert!(outcome.is_err(), "{kind} {size}"),
                }
            }
        }
    }
}
