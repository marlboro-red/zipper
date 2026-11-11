use std::{
    collections::VecDeque,
    io::{self, Read},
};

pub(crate) const LARGEST_DICTIONARY: u64 = 4 << 30;
const SMALLEST_WINDOW: u64 = 1 << 18;
const BLOCK_PADDING: usize = 32;
const QUICK_BITS: u32 = 10;
const LONGEST_CODE: usize = 15;
const BIT_LENGTH_SYMBOLS: usize = 20;
const MAIN_SYMBOLS: usize = 306;
const DISTANCE_SYMBOLS: usize = 64;
const LOW_DISTANCE_SYMBOLS: usize = 16;
const REPEAT_SYMBOLS: usize = 44;
const ALL_LENGTHS: usize = MAIN_SYMBOLS + DISTANCE_SYMBOLS + LOW_DISTANCE_SYMBOLS + REPEAT_SYMBOLS;
const FILTER_SYMBOL: u16 = 256;
const LAST_LENGTH_SYMBOL: u16 = 257;
const FIRST_REPEAT_SYMBOL: u16 = 258;
const FIRST_MATCH_SYMBOL: u16 = 262;
const LARGEST_FILTER: u64 = 0x40_0000;
const SMALLEST_FILTER: u64 = 4;
const MOST_FILTERS: usize = 8192;
const BLOCK_CHECK: u8 = 0x5A;
const LONGEST_BLOCK_SIZE: usize = 3;
const LAST_BLOCK: u8 = 0x40;
const HAS_TABLES: u8 = 0x80;
const E8_FILE_SIZE: u32 = 0x100_0000;
const ARM_BRANCH: u8 = 0xEB;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FilterKind {
    Delta { channels: u32 },
    E8,
    E8E9,
    Arm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Filter {
    kind: FilterKind,
    start: u64,
    length: u64,
}

pub(crate) struct Huffman {
    quick: Vec<u32>,
    limits: [u32; LONGEST_CODE + 1],
    first: [u32; LONGEST_CODE + 1],
    symbols: Vec<u16>,
}

impl Huffman {
    pub(crate) fn new(lengths: &[u8]) -> Result<Self, &'static str> {
        let mut counts = [0u32; LONGEST_CODE + 1];
        for &length in lengths {
            if usize::from(length) > LONGEST_CODE {
                return Err("RAR 5 code length is longer than 15 bits");
            }
            counts[usize::from(length)] += 1;
        }
        counts[0] = 0;
        let mut limits = [0u32; LONGEST_CODE + 1];
        let mut first = [0u32; LONGEST_CODE + 1];
        let mut code = 0u32;
        for length in 1..=LONGEST_CODE {
            code += counts[length] << (16 - length);
            if code > 1 << 16 {
                return Err("RAR 5 Huffman table is over-full");
            }
            limits[length] = code;
            first[length] = first[length - 1] + counts[length - 1];
        }
        let mut next = first;
        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length > 0 {
                let slot = &mut next[usize::from(length)];
                symbols[*slot as usize] = symbol as u16;
                *slot += 1;
            }
        }
        let mut table = Huffman {
            quick: Vec::new(),
            limits,
            first,
            symbols,
        };
        table.quick = (0..1u32 << QUICK_BITS)
            .map(|index| {
                let bits = index << (16 - QUICK_BITS);
                match table.slow(bits) {
                    Some((symbol, length)) if length <= QUICK_BITS => {
                        u32::from(symbol) << 8 | length
                    }
                    _ => 0,
                }
            })
            .collect();
        Ok(table)
    }

    fn slow(&self, bits: u32) -> Option<(u16, u32)> {
        let length = (1..=LONGEST_CODE).find(|&length| bits < self.limits[length])?;
        let offset = (bits - self.limits[length - 1]) >> (16 - length);
        let index = self.first[length] + offset;
        let symbol = *self.symbols.get(index as usize)?;
        Some((symbol, length as u32))
    }

    pub(crate) fn decode(&self, bits: &mut Bits) -> io::Result<u16> {
        let window = bits.peek() >> 16;
        let entry = self.quick[(window >> (16 - QUICK_BITS)) as usize];
        let (symbol, length) = if entry != 0 {
            ((entry >> 8) as u16, entry & 0xFF)
        } else {
            self.slow(window)
                .ok_or_else(|| invalid("RAR 5 stream holds a code its table leaves unused"))?
        };
        bits.skip(length);
        Ok(symbol)
    }
}

pub(crate) struct Bits<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Bits<'a> {
    pub(crate) fn new(data: &'a [u8], position: usize) -> Self {
        Bits { data, position }
    }

    pub(crate) fn position(&self) -> usize {
        self.position
    }

    pub(crate) fn peek(&self) -> u32 {
        let byte = self.position / 8;
        let mut word = [0u8; 8];
        if let Some(bytes) = self.data.get(byte..byte + 8) {
            word.copy_from_slice(bytes);
        } else if let Some(bytes) = self.data.get(byte..) {
            word[..bytes.len()].copy_from_slice(bytes);
        }
        ((u64::from_be_bytes(word) << (self.position % 8)) >> 32) as u32
    }

    pub(crate) fn skip(&mut self, count: u32) {
        self.position += count as usize;
    }

    pub(crate) fn take(&mut self, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        let value = self.peek() >> (32 - count);
        self.skip(count);
        value
    }
}

pub(crate) struct Tables {
    main: Huffman,
    distance: Huffman,
    low_distance: Huffman,
    repeat: Huffman,
}

pub(crate) fn read_tables(bits: &mut Bits) -> io::Result<Tables> {
    let mut lengths = [0u8; BIT_LENGTH_SYMBOLS];
    let mut filled = 0;
    while filled < BIT_LENGTH_SYMBOLS {
        let value = bits.take(4) as u8;
        if value == 15 {
            let zeros = bits.take(4) as usize;
            if zeros == 0 {
                lengths[filled] = 15;
                filled += 1;
            } else {
                filled = (filled + zeros + 2).min(BIT_LENGTH_SYMBOLS);
            }
        } else {
            lengths[filled] = value;
            filled += 1;
        }
    }
    let length_code = Huffman::new(&lengths).map_err(invalid)?;
    let mut all = [0u8; ALL_LENGTHS];
    let mut index = 0;
    while index < ALL_LENGTHS {
        let symbol = length_code.decode(bits)?;
        if symbol < 16 {
            all[index] = symbol as u8;
            index += 1;
            continue;
        }
        let repeat = match symbol {
            16 | 18 => 3 + bits.take(3) as usize,
            _ => 11 + bits.take(7) as usize,
        };
        let value = if symbol < 18 {
            *index
                .checked_sub(1)
                .and_then(|previous| all.get(previous))
                .ok_or_else(|| invalid("RAR 5 table repeats a length before the first"))?
        } else {
            0
        };
        let end = (index + repeat).min(ALL_LENGTHS);
        all[index..end].fill(value);
        index = end;
    }
    let (main, rest) = all.split_at(MAIN_SYMBOLS);
    let (distance, rest) = rest.split_at(DISTANCE_SYMBOLS);
    let (low_distance, repeat) = rest.split_at(LOW_DISTANCE_SYMBOLS);
    Ok(Tables {
        main: Huffman::new(main).map_err(invalid)?,
        distance: Huffman::new(distance).map_err(invalid)?,
        low_distance: Huffman::new(low_distance).map_err(invalid)?,
        repeat: Huffman::new(repeat).map_err(invalid)?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlockHeader {
    pub(crate) size: usize,
    pub(crate) header_bytes: usize,
    pub(crate) bits_in_last_byte: u32,
    pub(crate) last: bool,
    pub(crate) tables: bool,
}

pub(crate) fn block_header(bytes: &[u8]) -> Result<BlockHeader, &'static str> {
    let flags = *bytes.first().ok_or("truncated RAR 5 block header")?;
    let size_bytes = usize::from((flags >> 3) & 3) + 1;
    if size_bytes > LONGEST_BLOCK_SIZE {
        return Err("RAR 5 block size has more than 3 bytes");
    }
    let header_bytes = 2 + size_bytes;
    let fields = bytes
        .get(..header_bytes)
        .ok_or("truncated RAR 5 block header")?;
    let mut size = [0u8; 4];
    size[..size_bytes].copy_from_slice(&fields[2..]);
    let check = BLOCK_CHECK ^ flags ^ size[0] ^ size[1] ^ size[2];
    if check != fields[1] {
        return Err("RAR 5 block header fails its check");
    }
    Ok(BlockHeader {
        size: u32::from_le_bytes(size) as usize,
        header_bytes,
        bits_in_last_byte: u32::from(flags & 7) + 1,
        last: flags & LAST_BLOCK != 0,
        tables: flags & HAS_TABLES != 0,
    })
}

pub(crate) fn match_length(bits: &mut Bits, code: u16) -> u32 {
    if code < 8 {
        return 2 + u32::from(code);
    }
    let extra = u32::from(code) / 4 - 1;
    2 + ((4 | (u32::from(code) & 3)) << extra) + bits.take(extra)
}

fn distance(bits: &mut Bits, tables: &Tables, slot: u16) -> io::Result<u64> {
    if slot < 4 {
        return Ok(1 + u64::from(slot));
    }
    let extra = u32::from(slot) / 2 - 1;
    let mut distance = 1 + (u64::from(2 | (slot & 1)) << extra);
    if extra < 4 {
        return Ok(distance + u64::from(bits.take(extra)));
    }
    if extra > 4 {
        distance += u64::from(bits.take(extra - 4)) << 4;
    }
    Ok(distance + u64::from(tables.low_distance.decode(bits)?))
}

fn length_bonus(distance: u64) -> u32 {
    u32::from(distance > 0x100) + u32::from(distance > 0x2000) + u32::from(distance > 0x4_0000)
}

fn filter_number(bits: &mut Bits) -> u64 {
    let bytes = bits.take(2) + 1;
    (0..bytes).fold(0, |value, byte| {
        value | u64::from(bits.take(8)) << (8 * byte)
    })
}

pub(crate) fn filter_kind(bits: &mut Bits) -> io::Result<FilterKind> {
    Ok(match bits.take(3) {
        0 => FilterKind::Delta {
            channels: bits.take(5) + 1,
        },
        1 => FilterKind::E8,
        2 => FilterKind::E8E9,
        3 => FilterKind::Arm,
        _ => return Err(invalid("RAR 5 filter type is not one RAR 5 defines")),
    })
}

pub(crate) fn run_filter(kind: FilterKind, data: &mut [u8], file_position: u64) {
    match kind {
        FilterKind::Delta { channels } => {
            let source = data.to_vec();
            let mut next = source.iter();
            for channel in 0..channels as usize {
                let mut previous = 0u8;
                for at in (channel..data.len()).step_by(channels as usize) {
                    previous = previous.wrapping_sub(*next.next().unwrap_or(&0));
                    data[at] = previous;
                }
            }
        }
        FilterKind::E8 | FilterKind::E8E9 => {
            let both = kind == FilterKind::E8E9;
            let mut at = 0usize;
            while at + 4 < data.len() {
                let opcode = data[at];
                at += 1;
                if opcode == 0xE8 || (both && opcode == 0xE9) {
                    let offset =
                        (file_position.wrapping_add(at as u64) % u64::from(E8_FILE_SIZE)) as u32;
                    let field: &mut [u8; 4] = (&mut data[at..at + 4]).try_into().unwrap();
                    let address = u32::from_le_bytes(*field);
                    if address & 0x8000_0000 != 0 {
                        if address.wrapping_add(offset) & 0x8000_0000 == 0 {
                            *field = address.wrapping_add(E8_FILE_SIZE).to_le_bytes();
                        }
                    } else if address.wrapping_sub(E8_FILE_SIZE) & 0x8000_0000 != 0 {
                        *field = address.wrapping_sub(offset).to_le_bytes();
                    }
                    at += 4;
                }
            }
        }
        FilterKind::Arm => {
            let mut at = 0usize;
            while at + 3 < data.len() {
                if data[at + 3] == ARM_BRANCH {
                    let field: &mut [u8; 4] = (&mut data[at..at + 4]).try_into().unwrap();
                    let word = u32::from_le_bytes(*field);
                    let step = (file_position.wrapping_add(at as u64) / 4) as u32;
                    let target = (word & 0x00FF_FFFF).wrapping_sub(step);
                    *field = ((target & 0x00FF_FFFF) | 0xEB00_0000).to_le_bytes();
                }
                at += 4;
            }
        }
    }
}

pub(crate) struct Stream {
    window: Vec<u8>,
    mask: u64,
    capacity: u64,
    written: u64,
    dictionary: u64,
    tables: Option<Tables>,
    distances: [u64; 4],
    last_length: u32,
    file_start: u64,
    file_size: u64,
    flushed: u64,
    filters: VecDeque<Filter>,
    last_filter_end: u64,
    filtered: Vec<u8>,
    filtered_read: usize,
    block: Vec<u8>,
    bit: usize,
    end_bit: usize,
    block_done: bool,
    block_last: bool,
    started: bool,
}

impl Default for Stream {
    fn default() -> Self {
        Stream {
            window: Vec::new(),
            mask: 0,
            capacity: 0,
            written: 0,
            dictionary: 0,
            tables: None,
            distances: [0; 4],
            last_length: 0,
            file_start: 0,
            file_size: 0,
            flushed: 0,
            filters: VecDeque::new(),
            last_filter_end: 0,
            filtered: Vec::new(),
            filtered_read: 0,
            block: Vec::new(),
            bit: 0,
            end_bit: 0,
            block_done: true,
            block_last: false,
            started: false,
        }
    }
}

impl Stream {
    pub(crate) fn start_file(&mut self, dictionary: u64, size: u64) -> Result<(), &'static str> {
        if dictionary > LARGEST_DICTIONARY {
            return Err("RAR 5 dictionary is larger than 4 GiB");
        }
        let capacity = dictionary.max(SMALLEST_WINDOW).next_power_of_two();
        *self = Stream {
            window: std::mem::take(&mut self.window),
            block: std::mem::take(&mut self.block),
            filtered: std::mem::take(&mut self.filtered),
            ..Stream::default()
        };
        self.window.clear();
        self.capacity = capacity;
        self.mask = capacity - 1;
        self.started = true;
        self.begin_file(dictionary, size);
        Ok(())
    }

    pub(crate) fn continue_solid_file(
        &mut self,
        dictionary: u64,
        size: u64,
    ) -> Result<(), &'static str> {
        if !self.started {
            return Err("RAR 5 solid file has no stream before it");
        }
        if dictionary > self.capacity {
            return Err("RAR 5 solid file needs a larger dictionary than its stream");
        }
        self.begin_file(dictionary, size);
        Ok(())
    }

    fn begin_file(&mut self, dictionary: u64, size: u64) {
        self.dictionary = dictionary;
        self.file_start = self.written;
        self.file_size = size;
        self.flushed = self.written;
        self.filters.clear();
        self.last_filter_end = 0;
        self.filtered.clear();
        self.filtered_read = 0;
        self.block_done = true;
        self.block_last = false;
    }

    pub(crate) fn reader<R: Read>(&mut self, packed: R, packed_size: u64) -> FileReader<'_, R> {
        FileReader {
            stream: self,
            packed,
            remaining: packed_size,
        }
    }

    fn file_written(&self) -> u64 {
        self.written - self.file_start
    }

    fn grow_for(&mut self, end: u64) -> io::Result<()> {
        let have = self.window.len() as u64;
        if end <= have || have == self.capacity {
            return Ok(());
        }
        let wanted = end.max(have * 2).min(self.capacity);
        let extra = (wanted - have) as usize;
        self.window.try_reserve_exact(extra).map_err(|_| {
            io::Error::new(
                io::ErrorKind::OutOfMemory,
                "RAR 5 window does not fit in memory",
            )
        })?;
        self.window.resize(wanted as usize, 0);
        Ok(())
    }

    fn put(&mut self, byte: u8) -> io::Result<()> {
        if self.file_written() >= self.file_size {
            return Err(invalid("RAR 5 stream decodes past the declared size"));
        }
        self.grow_for(self.written + 1)?;
        let at = (self.written & self.mask) as usize;
        self.window[at] = byte;
        self.written += 1;
        Ok(())
    }

    fn copy(&mut self, distance: u64, length: u32) -> io::Result<()> {
        let length = u64::from(length);
        if distance == 0 || distance > self.dictionary.max(1) || distance > self.written {
            return Err(invalid(
                "RAR 5 match reaches before its stream or dictionary",
            ));
        }
        if length > self.file_size - self.file_written() {
            return Err(invalid("RAR 5 stream decodes past the declared size"));
        }
        self.grow_for(self.written + length)?;
        let mask = self.mask;
        let to = self.written & mask;
        let from = (self.written - distance) & mask;
        if distance >= length && to + length <= self.capacity && from + length <= self.capacity {
            let (to, from, length) = (to as usize, from as usize, length as usize);
            self.window.copy_within(from..from + length, to);
        } else {
            for step in 0..length {
                let byte = self.window[((self.written - distance + step) & mask) as usize];
                self.window[((self.written + step) & mask) as usize] = byte;
            }
        }
        self.written += length;
        Ok(())
    }

    fn add_filter(&mut self, bits: &mut Bits) -> io::Result<()> {
        let start = filter_number(bits);
        let length = filter_number(bits);
        let kind = filter_kind(bits)?;
        let start = self.file_written() + start;
        if !(SMALLEST_FILTER..=LARGEST_FILTER).contains(&length)
            || length > self.dictionary / 2
            || start < self.last_filter_end
            || start + length > self.file_size
            || self.filters.len() >= MOST_FILTERS
        {
            return Err(invalid("RAR 5 filter is outside its file or window"));
        }
        self.last_filter_end = start + length;
        self.filters.push_back(Filter {
            kind,
            start,
            length,
        });
        Ok(())
    }

    fn decode_symbols(&mut self) -> io::Result<()> {
        let block = std::mem::take(&mut self.block);
        let result = self.decode_from(&block);
        self.block = block;
        result
    }

    fn decode_from(&mut self, block: &[u8]) -> io::Result<()> {
        let tables = self
            .tables
            .take()
            .ok_or_else(|| invalid("RAR 5 block has no tables before it"))?;
        let mut bits = Bits::new(block, self.bit);
        let half = self.capacity / 2;
        let result = loop {
            if bits.position() > self.end_bit {
                break Err(invalid("RAR 5 block is read past its end"));
            }
            if bits.position() == self.end_bit {
                self.block_done = true;
                break Ok(());
            }
            if self.written - self.flushed > half {
                break Ok(());
            }
            let symbol = match tables.main.decode(&mut bits) {
                Ok(symbol) => symbol,
                Err(error) => break Err(error),
            };
            let step = match symbol {
                0..=255 => self.put(symbol as u8),
                FILTER_SYMBOL => self.add_filter(&mut bits),
                LAST_LENGTH_SYMBOL => {
                    if self.last_length == 0 {
                        Ok(())
                    } else {
                        self.copy(self.distances[0], self.last_length)
                    }
                }
                FIRST_REPEAT_SYMBOL..FIRST_MATCH_SYMBOL => {
                    let index = usize::from(symbol - FIRST_REPEAT_SYMBOL);
                    let distance = self.distances[index];
                    self.distances.copy_within(0..index, 1);
                    self.distances[0] = distance;
                    tables.repeat.decode(&mut bits).and_then(|code| {
                        let length = match_length(&mut bits, code);
                        self.last_length = length;
                        self.copy(distance, length)
                    })
                }
                _ => {
                    let length = match_length(&mut bits, symbol - FIRST_MATCH_SYMBOL);
                    tables
                        .distance
                        .decode(&mut bits)
                        .and_then(|slot| distance(&mut bits, &tables, slot))
                        .and_then(|distance| {
                            let length = length + length_bonus(distance);
                            self.distances.copy_within(0..3, 1);
                            self.distances[0] = distance;
                            self.last_length = length;
                            self.copy(distance, length)
                        })
                }
            };
            if let Err(error) = step {
                break Err(error);
            }
        };
        self.bit = bits.position();
        self.tables = Some(tables);
        result
    }

    fn next_block<R: Read>(&mut self, packed: &mut R, remaining: &mut u64) -> io::Result<()> {
        if self.block_last {
            return Err(invalid("RAR 5 stream ends before its declared size"));
        }
        let mut head = [0u8; 5];
        let first = 3.min(*remaining as usize);
        packed
            .read_exact(&mut head[..first])
            .map_err(|_| invalid("truncated RAR 5 block header"))?;
        let size_bytes = usize::from((head[0] >> 3) & 3) + 1;
        if size_bytes > LONGEST_BLOCK_SIZE {
            return Err(invalid("RAR 5 block size has more than 3 bytes"));
        }
        let header_bytes = 2 + size_bytes;
        if header_bytes as u64 > *remaining {
            return Err(invalid("truncated RAR 5 block header"));
        }
        if header_bytes > first {
            packed
                .read_exact(&mut head[first..header_bytes])
                .map_err(|_| invalid("truncated RAR 5 block header"))?;
        }
        let header = block_header(&head[..header_bytes]).map_err(invalid)?;
        *remaining -= header_bytes as u64;
        if header.size as u64 > *remaining {
            return Err(invalid("RAR 5 block runs past its file's packed data"));
        }
        self.block.clear();
        self.block.resize(header.size + BLOCK_PADDING, 0);
        packed
            .read_exact(&mut self.block[..header.size])
            .map_err(|_| invalid("truncated RAR 5 block"))?;
        *remaining -= header.size as u64;
        self.bit = 0;
        self.end_bit = match header.size {
            0 => 0,
            size => (size - 1) * 8 + header.bits_in_last_byte as usize,
        };
        self.block_done = false;
        self.block_last = header.last;
        if header.last && *remaining != 0 {
            return Err(invalid("RAR 5 file holds data after its last block"));
        }
        if header.tables {
            let block = std::mem::take(&mut self.block);
            let mut bits = Bits::new(&block, 0);
            let tables = read_tables(&mut bits);
            self.bit = bits.position();
            self.block = block;
            self.tables = Some(tables?);
            if self.bit > self.end_bit {
                return Err(invalid("RAR 5 tables run past their block"));
            }
        }
        Ok(())
    }

    fn ready_end(&self) -> u64 {
        match self.filters.front() {
            Some(filter) => self.written.min(self.file_start + filter.start),
            None => self.written,
        }
    }

    fn copy_out(&mut self, output: &mut [u8]) -> usize {
        let end = self.ready_end();
        let mut copied = 0;
        while self.flushed < end && copied < output.len() {
            let at = self.flushed & self.mask;
            let run = (end - self.flushed)
                .min(self.capacity - at)
                .min((output.len() - copied) as u64) as usize;
            let at = at as usize;
            output[copied..copied + run].copy_from_slice(&self.window[at..at + run]);
            copied += run;
            self.flushed += run as u64;
        }
        copied
    }

    fn run_ready_filter(&mut self) -> io::Result<bool> {
        let Some(&filter) = self.filters.front() else {
            return Ok(false);
        };
        let start = self.file_start + filter.start;
        if self.flushed != start || self.written < start + filter.length {
            return Ok(false);
        }
        self.filters.pop_front();
        self.filtered.clear();
        self.filtered
            .try_reserve_exact(filter.length as usize)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "RAR 5 filter does not fit in memory",
                )
            })?;
        for step in 0..filter.length {
            self.filtered
                .push(self.window[((start + step) & self.mask) as usize]);
        }
        run_filter(filter.kind, &mut self.filtered, filter.start);
        self.filtered_read = 0;
        self.flushed += filter.length;
        Ok(true)
    }
}

pub(crate) struct FileReader<'a, R> {
    stream: &'a mut Stream,
    packed: R,
    remaining: u64,
}

impl<R: Read> FileReader<'_, R> {
    fn file_end(&self) -> u64 {
        self.stream.file_start + self.stream.file_size
    }

    fn decode_more(&mut self) -> io::Result<()> {
        if self.stream.block_done {
            if self.remaining == 0 {
                return Err(invalid("RAR 5 stream ends before its declared size"));
            }
            self.stream
                .next_block(&mut self.packed, &mut self.remaining)?;
        }
        self.stream.decode_symbols()
    }

    pub(crate) fn finish_at_declared_size(mut self) -> io::Result<()> {
        if self.stream.flushed != self.file_end()
            || self.stream.filtered_read < self.stream.filtered.len()
        {
            return Err(invalid("RAR 5 stream ends before its declared size"));
        }
        while !(self.stream.block_done && self.remaining == 0) {
            let before = self.stream.written;
            self.decode_more()?;
            if self.stream.written != before || !self.stream.filters.is_empty() {
                return Err(invalid("RAR 5 stream decodes past the declared size"));
            }
        }
        if !self.stream.filters.is_empty() {
            return Err(invalid("RAR 5 filter is left over at the end of its file"));
        }
        Ok(())
    }
}

impl<R: Read> Read for FileReader<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut copied = 0;
        while copied < output.len() {
            let stream = &mut *self.stream;
            if stream.filtered_read < stream.filtered.len() {
                let run = (stream.filtered.len() - stream.filtered_read).min(output.len() - copied);
                output[copied..copied + run].copy_from_slice(
                    &stream.filtered[stream.filtered_read..stream.filtered_read + run],
                );
                stream.filtered_read += run;
                copied += run;
                continue;
            }
            let plain = stream.copy_out(&mut output[copied..]);
            if plain > 0 {
                copied += plain;
                continue;
            }
            if stream.run_ready_filter()? {
                continue;
            }
            if copied > 0 || stream.flushed == self.file_end() {
                break;
            }
            self.decode_more()?;
        }
        Ok(copied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_headers_check_their_sum_and_size_bytes() {
        let flags = 0x80 | 0x40 | (1 << 3) | 6;
        let size = [0x34u8, 0x12];
        let check = BLOCK_CHECK ^ flags ^ size[0] ^ size[1];
        let header = block_header(&[flags, check, size[0], size[1]]).unwrap();
        assert_eq!(
            header,
            BlockHeader {
                size: 0x1234,
                header_bytes: 4,
                bits_in_last_byte: 7,
                last: true,
                tables: true,
            }
        );
        assert!(block_header(&[flags, check ^ 1, size[0], size[1]]).is_err());
        assert!(block_header(&[0x18, 0, 0, 0, 0]).is_err());
        assert!(block_header(&[flags, check, size[0]]).is_err());
    }

    #[test]
    fn huffman_tables_decode_every_code_as_a_bit_at_a_time_walk() {
        let lengths = [2u8, 3, 3, 0, 4, 4, 4, 4, 2, 15, 15];
        let table = Huffman::new(&lengths[..9]).unwrap();
        let mut code = 0u32;
        let mut codes = Vec::new();
        for length in 1..=15u8 {
            for (symbol, _) in lengths[..9]
                .iter()
                .enumerate()
                .filter(|(_, l)| **l == length)
            {
                codes.push((symbol as u16, length, code));
                code += 1;
            }
            code <<= 1;
        }
        for (symbol, length, code) in codes {
            let word = (code << (32 - u32::from(length))).to_be_bytes();
            let mut bits = Bits::new(&word, 0);
            assert_eq!(table.decode(&mut bits).unwrap(), symbol);
            assert_eq!(bits.position(), usize::from(length));
        }
        assert!(Huffman::new(&[1, 1, 1]).is_err());
        let incomplete = Huffman::new(&[1, 0, 2]).unwrap();
        let mut bits = Bits::new(&[0xC0, 0, 0, 0], 0);
        assert!(incomplete.decode(&mut bits).is_err());
    }

    #[test]
    fn delta_filters_undo_a_per_channel_difference() {
        for channels in 1..=4u32 {
            let original: Vec<u8> = (0..40u32).map(|i| (i * i + 3 * i) as u8).collect();
            let mut encoded = Vec::new();
            for channel in 0..channels as usize {
                let mut previous = 0u8;
                for at in (channel..original.len()).step_by(channels as usize) {
                    encoded.push(previous.wrapping_sub(original[at]));
                    previous = original[at];
                }
            }
            run_filter(FilterKind::Delta { channels }, &mut encoded, 0);
            assert_eq!(encoded, original, "{channels} channels");
        }
    }

    #[test]
    fn e8_filters_turn_absolute_calls_back_into_relative_ones() {
        let mut data = vec![0x90, 0xE8, 0x10, 0x00, 0x00, 0x00, 0x90, 0x90, 0x90];
        run_filter(FilterKind::E8, &mut data, 0);
        assert_eq!(&data[2..6], &(0x10u32 - 2).to_le_bytes());
        let mut jump = vec![0xE9, 0x10, 0x00, 0x00, 0x00, 0x90];
        let unchanged = jump.clone();
        run_filter(FilterKind::E8, &mut jump, 0);
        assert_eq!(jump, unchanged);
        run_filter(FilterKind::E8E9, &mut jump, 0);
        assert_eq!(&jump[1..5], &(0x10u32 - 1).to_le_bytes());
    }

    #[test]
    fn arm_filters_rewrite_only_branch_and_link_words() {
        let mut data = vec![0x10, 0, 0, 0xEB, 0x10, 0, 0, 0xEA];
        run_filter(FilterKind::Arm, &mut data, 8);
        assert_eq!(&data[..4], &[0x0E, 0, 0, 0xEB]);
        assert_eq!(&data[4..], &[0x10, 0, 0, 0xEA]);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn block_headers_never_panic_and_read_only_their_own_bytes() {
        let bytes: [u8; 6] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        if let Ok(header) = block_header(&bytes[..length]) {
            assert!((3..=5).contains(&header.header_bytes) && header.header_bytes <= length);
            assert!(header.size < 1 << 24);
            assert!((1..=8).contains(&header.bits_in_last_byte));
        }
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    const MAIN_BITS: u32 = 9;
    const DISTANCE_BITS: u32 = 6;
    const LOW_DISTANCE_BITS: u32 = 4;
    const REPEAT_BITS: u32 = 6;

    enum Token {
        Literal(u8),
        Match { length: u32, distance: u64 },
        Repeat { index: u16, length: u32 },
        Last,
        Filter { start: u32, length: u32, kind: u32 },
    }

    #[derive(Default)]
    struct Writer {
        bytes: Vec<u8>,
        bits: usize,
    }

    impl Writer {
        fn put(&mut self, value: u64, count: u32) {
            for bit in (0..count).rev() {
                if self.bits.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                if value >> bit & 1 == 1 {
                    *self.bytes.last_mut().unwrap() |= 0x80 >> (self.bits % 8);
                }
                self.bits += 1;
            }
        }
    }

    fn length_code(length: u32) -> (u16, u32, u32) {
        if length < 10 {
            return ((length - 2) as u16, 0, 0);
        }
        (8..44u16)
            .find_map(|code| {
                let extra = u32::from(code) / 4 - 1;
                let base = 2 + ((4 | (u32::from(code) & 3)) << extra);
                (length >= base && length - base < 1 << extra).then_some((
                    code,
                    extra,
                    length - base,
                ))
            })
            .unwrap()
    }

    fn distance_code(distance: u64) -> (u16, u32, u64) {
        if distance <= 4 {
            return ((distance - 1) as u16, 0, 0);
        }
        (4..64u16)
            .find_map(|slot| {
                let extra = u32::from(slot) / 2 - 1;
                let base = 1 + (u64::from(2 | (slot & 1)) << extra);
                (distance >= base && distance - base < 1 << extra).then_some((
                    slot,
                    extra,
                    distance - base,
                ))
            })
            .unwrap()
    }

    fn filter_number(writer: &mut Writer, value: u32) {
        writer.put(3, 2);
        for byte in 0..4 {
            writer.put(u64::from(value >> (8 * byte) & 0xFF), 8);
        }
    }

    fn block_with_fixed_width_tables(tokens: &[Token]) -> Vec<u8> {
        let mut writer = Writer::default();
        for _ in 0..BIT_LENGTH_SYMBOLS {
            writer.put(5, 4);
        }
        for (count, bits) in [
            (MAIN_SYMBOLS, MAIN_BITS),
            (DISTANCE_SYMBOLS, DISTANCE_BITS),
            (LOW_DISTANCE_SYMBOLS, LOW_DISTANCE_BITS),
            (REPEAT_SYMBOLS, REPEAT_BITS),
        ] {
            for _ in 0..count {
                writer.put(u64::from(bits), 5);
            }
        }
        for token in tokens {
            match *token {
                Token::Literal(byte) => writer.put(u64::from(byte), MAIN_BITS),
                Token::Match { length, distance } => {
                    let length = length - length_bonus(distance);
                    let (code, extra, value) = length_code(length);
                    writer.put(u64::from(FIRST_MATCH_SYMBOL + code), MAIN_BITS);
                    writer.put(u64::from(value), extra);
                    let (slot, extra, value) = distance_code(distance);
                    writer.put(u64::from(slot), DISTANCE_BITS);
                    if extra < 4 {
                        writer.put(value, extra);
                    } else {
                        writer.put(value >> 4, extra - 4);
                        writer.put(value & 15, LOW_DISTANCE_BITS);
                    }
                }
                Token::Repeat { index, length } => {
                    writer.put(u64::from(FIRST_REPEAT_SYMBOL + index), MAIN_BITS);
                    let (code, extra, value) = length_code(length);
                    writer.put(u64::from(code), REPEAT_BITS);
                    writer.put(u64::from(value), extra);
                }
                Token::Last => writer.put(u64::from(LAST_LENGTH_SYMBOL), MAIN_BITS),
                Token::Filter {
                    start,
                    length,
                    kind,
                } => {
                    writer.put(u64::from(FILTER_SYMBOL), MAIN_BITS);
                    filter_number(&mut writer, start);
                    filter_number(&mut writer, length);
                    writer.put(u64::from(kind), 3);
                    if kind == 0 {
                        writer.put(0, 5);
                    }
                }
            }
        }
        let size = writer.bytes.len();
        let last_bits = match writer.bits % 8 {
            0 => 8,
            bits => bits,
        };
        let size_bytes = size.to_le_bytes();
        let flags = 0x80 | 0x40 | (2 << 3) | (last_bits as u8 - 1);
        let check = BLOCK_CHECK ^ flags ^ size_bytes[0] ^ size_bytes[1] ^ size_bytes[2];
        let mut out = vec![flags, check, size_bytes[0], size_bytes[1], size_bytes[2]];
        out.extend(writer.bytes);
        out
    }

    fn decode(packed: &[u8], size: u64) -> io::Result<Vec<u8>> {
        let mut stream = Stream::default();
        stream.start_file(128 << 10, size).map_err(invalid)?;
        let mut reader = stream.reader(packed, packed.len() as u64);
        let mut out = Vec::new();
        let mut buffer = [0u8; 7];
        while (out.len() as u64) < size {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                return Err(invalid("short"));
            }
            out.extend_from_slice(&buffer[..read]);
        }
        reader.finish_at_declared_size()?;
        Ok(out)
    }

    #[test]
    fn literals_matches_and_repeats_decode_as_a_byte_at_a_time_model() {
        let tokens = [
            Token::Literal(b'a'),
            Token::Literal(b'b'),
            Token::Literal(b'c'),
            Token::Match {
                length: 7,
                distance: 3,
            },
            Token::Last,
            Token::Literal(b'x'),
            Token::Repeat {
                index: 0,
                length: 5,
            },
            Token::Match {
                length: 40,
                distance: 300,
            },
        ];
        let mut model: Vec<u8> = Vec::new();
        let encoded = &tokens[..7];
        let mut distances = [0usize; 4];
        let mut last = 0usize;
        let copy = |model: &mut Vec<u8>, distance: usize, length: usize| {
            for _ in 0..length {
                model.push(model[model.len() - distance]);
            }
        };
        for token in encoded {
            match *token {
                Token::Literal(byte) => model.push(byte),
                Token::Match { length, distance } => {
                    distances = [distance as usize, distances[0], distances[1], distances[2]];
                    last = length as usize;
                    copy(&mut model, distance as usize, length as usize);
                }
                Token::Repeat { index, length } => {
                    let distance = distances[usize::from(index)];
                    distances.copy_within(0..usize::from(index), 1);
                    distances[0] = distance;
                    last = length as usize;
                    copy(&mut model, distance, length as usize);
                }
                Token::Last => copy(&mut model, distances[0], last),
                Token::Filter { .. } => {}
            }
        }
        let packed = block_with_fixed_width_tables(&tokens[..7]);
        assert_eq!(decode(&packed, model.len() as u64).unwrap(), model);
        assert!(
            decode(
                &block_with_fixed_width_tables(&tokens),
                model.len() as u64 + 40
            )
            .is_err()
        );
    }

    #[test]
    fn a_match_before_the_start_of_the_stream_is_refused() {
        let packed = block_with_fixed_width_tables(&[
            Token::Literal(1),
            Token::Match {
                length: 4,
                distance: 2,
            },
        ]);
        assert!(decode(&packed, 5).is_err());
        let fits = block_with_fixed_width_tables(&[
            Token::Literal(1),
            Token::Literal(2),
            Token::Match {
                length: 4,
                distance: 2,
            },
        ]);
        assert_eq!(decode(&fits, 6).unwrap(), [1, 2, 1, 2, 1, 2]);
    }

    #[test]
    fn a_stream_that_decodes_past_or_short_of_its_size_is_refused() {
        let packed = block_with_fixed_width_tables(&[
            Token::Literal(1),
            Token::Literal(2),
            Token::Literal(3),
        ]);
        assert_eq!(decode(&packed, 3).unwrap(), [1, 2, 3]);
        assert!(decode(&packed, 2).is_err());
        assert!(decode(&packed, 4).is_err());
    }

    #[test]
    fn filters_run_on_their_block_and_one_outside_the_file_is_refused() {
        let mut tokens = vec![Token::Filter {
            start: 0,
            length: 8,
            kind: 1,
        }];
        let data = [0xE8u8, 0x10, 0, 0, 0, 0x90, 0x90, 0x90];
        tokens.extend(data.iter().map(|&byte| Token::Literal(byte)));
        let mut wanted = data;
        run_filter(FilterKind::E8, &mut wanted, 0);
        assert_eq!(
            decode(&block_with_fixed_width_tables(&tokens), 8).unwrap(),
            wanted
        );
        tokens[0] = Token::Filter {
            start: 4,
            length: 8,
            kind: 1,
        };
        assert!(decode(&block_with_fixed_width_tables(&tokens), 8).is_err());
        tokens[0] = Token::Filter {
            start: 0,
            length: 8,
            kind: 4,
        };
        assert!(decode(&block_with_fixed_width_tables(&tokens), 8).is_err());
    }

    #[test]
    fn a_block_header_that_asks_for_a_four_byte_size_is_refused() {
        let mut packed = block_with_fixed_width_tables(&[Token::Literal(1)]);
        packed[0] |= 3 << 3;
        packed.extend([0; 8]);
        assert!(decode(&packed, 1).is_err());
    }

    #[test]
    fn a_cut_block_or_a_block_read_past_its_end_is_refused() {
        let packed = block_with_fixed_width_tables(&[Token::Literal(1), Token::Literal(2)]);
        for cut in 0..packed.len() {
            assert!(decode(&packed[..cut], 2).is_err(), "cut to {cut}");
        }
        let mut short = packed.clone();
        short[0] &= !7;
        short[1] = BLOCK_CHECK ^ short[0] ^ short[2] ^ short[3] ^ short[4];
        assert!(decode(&short, 2).is_err());
    }
}
