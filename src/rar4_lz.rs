use crate::rar5_lz::{Bits, Huffman};
use std::{
    cell::RefCell,
    collections::VecDeque,
    io::{self, Read},
    rc::Rc,
};

pub(crate) const WINDOW: u64 = 4 << 20;
const INPUT_CHUNK: usize = 64 * 1024;
const BATCH: u64 = 1 << 20;
const LOOKAHEAD: usize = 64;
const MAIN_SYMBOLS: usize = 299;
const OFFSET_SYMBOLS: usize = 60;
const LOW_OFFSET_SYMBOLS: usize = 17;
const LENGTH_SYMBOLS: usize = 28;
const ALL_LENGTHS: usize = MAIN_SYMBOLS + OFFSET_SYMBOLS + LOW_OFFSET_SYMBOLS + LENGTH_SYMBOLS;
const BIT_LENGTH_SYMBOLS: usize = 20;
const VM_MEMORY: usize = 0x4_0000;
const VM_WORK: usize = 0x3_C000;
const VM_USER_GLOBAL: u64 = 0x2000 - 0x40;
const LONGEST_PROGRAM: u64 = 0x1_0000;
const MOST_FILTERS: usize = 8192;
const MOST_PROGRAMS: usize = 1024;
const E8_FILE_SIZE: u32 = 0x100_0000;
pub(crate) const LARGEST_PPMD_MEMORY: u32 = 256 << 20;
const PPMD_MODEL_ALLOWANCE: u64 = 4 * LARGEST_PPMD_MEMORY as u64;
const PPMD_MODEL_BYTES_PER_OUTPUT: u64 = 64;
const LOW_OFFSET_REPEAT: u16 = 16;
const LOW_OFFSET_REPEATS: u32 = 15;
const DEFAULT_ESCAPE: u8 = 2;

const LENGTH_BASES: [u32; LENGTH_SYMBOLS] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128,
    160, 192, 224,
];
const LENGTH_BITS: [u32; LENGTH_SYMBOLS] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5,
];
const OFFSET_BASES: [u32; OFFSET_SYMBOLS] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536,
    2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576, 32768, 49152, 65536, 98304, 131072, 196608,
    262144, 327680, 393216, 458752, 524288, 589824, 655360, 720896, 786432, 851968, 917504, 983040,
    1048576, 1310720, 1572864, 1835008, 2097152, 2359296, 2621440, 2883584, 3145728, 3407872,
    3670016, 3932160,
];
const OFFSET_BITS: [u32; OFFSET_SYMBOLS] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13, 14, 14, 15, 15, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 18, 18, 18, 18, 18,
    18, 18, 18, 18, 18, 18, 18,
];
const SHORT_BASES: [u32; 8] = [0, 4, 8, 16, 32, 64, 128, 192];
const SHORT_BITS: [u32; 8] = [2, 2, 3, 4, 5, 6, 6, 6];

// EXTERNAL(WinRAR): the CRC-32 of each standard filter program WinRAR
// embeds, with its length in the high 32 bits, as libarchive matches them.
const DELTA_PROGRAM: u64 = 0x1D_0E06_077D;
const E8_PROGRAM: u64 = 0x35_AD57_6887;
const E8E9_PROGRAM: u64 = 0x39_3CD7_E57E;
const RGB_PROGRAM: u64 = 0x95_1C2C_5DC8;
const AUDIO_PROGRAM: u64 = 0xD8_BC85_E701;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) struct Input<R> {
    reader: R,
    remaining: u64,
    buffer: Vec<u8>,
    bit: usize,
}

impl<R: Read> Input<R> {
    fn new(reader: R, packed: u64) -> Self {
        Input {
            reader,
            remaining: packed,
            buffer: Vec::new(),
            bit: 0,
        }
    }

    fn fill(&mut self) -> io::Result<()> {
        let byte = self.bit / 8;
        if self.buffer.len() - byte.min(self.buffer.len()) >= LOOKAHEAD || self.remaining == 0 {
            return Ok(());
        }
        let used = byte.min(self.buffer.len());
        self.buffer.drain(..used);
        self.bit -= used * 8;
        let take = self.remaining.min(INPUT_CHUNK as u64) as usize;
        let start = self.buffer.len();
        self.buffer.resize(start + take, 0);
        self.reader
            .read_exact(&mut self.buffer[start..])
            .map_err(|_| invalid("truncated RAR 4 data"))?;
        self.remaining -= take as u64;
        Ok(())
    }

    fn check(&self) -> io::Result<()> {
        if self.remaining == 0 && self.bit > self.buffer.len() * 8 {
            return Err(invalid("RAR 4 stream is read past its packed data"));
        }
        Ok(())
    }

    fn with_bits<T>(&mut self, read: impl FnOnce(&mut Bits) -> T) -> io::Result<T> {
        self.fill()?;
        let mut bits = Bits::new(&self.buffer, self.bit);
        let value = read(&mut bits);
        self.bit = bits.position();
        self.check()?;
        Ok(value)
    }

    fn bits(&mut self, count: u32) -> io::Result<u32> {
        self.with_bits(|bits| bits.take(count))
    }

    fn symbol(&mut self, table: &Huffman) -> io::Result<u16> {
        self.with_bits(|bits| table.decode(bits))?
    }

    fn align(&mut self) {
        self.bit = self.bit.div_ceil(8) * 8;
    }

    fn byte(&mut self) -> io::Result<u8> {
        if !self.bit.is_multiple_of(8) {
            return Err(invalid("RAR 4 PPMd data does not start on a byte"));
        }
        self.fill()?;
        let byte = *self
            .buffer
            .get(self.bit / 8)
            .ok_or_else(|| invalid("truncated RAR 4 PPMd data"))?;
        self.bit += 8;
        Ok(byte)
    }
}

struct PpmdInput<R>(Rc<RefCell<Input<R>>>);

impl<R: Read> Read for PpmdInput<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut input = self.0.borrow_mut();
        for (index, slot) in output.iter_mut().enumerate() {
            match input.byte() {
                Ok(byte) => *slot = byte,
                Err(_) if index > 0 => return Ok(index),
                Err(error) => return Err(error),
            }
        }
        Ok(output.len())
    }
}

struct Tables {
    main: Huffman,
    offset: Huffman,
    low_offset: Huffman,
    length: Huffman,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FilterKind {
    Delta,
    E8,
    E8E9,
    Rgb,
    Audio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Filter {
    kind: FilterKind,
    start: u64,
    length: u64,
    registers: [u32; 7],
}

struct Program {
    kind: Option<FilterKind>,
    old_length: u64,
}

pub(crate) fn program_kind(code: &[u8]) -> Result<Option<FilterKind>, &'static str> {
    let (&check, rest) = code.split_first().ok_or("RAR 4 filter program is empty")?;
    if rest.iter().fold(0, |sum, byte| sum ^ byte) != check {
        return Err("RAR 4 filter program fails its check");
    }
    let fingerprint = u64::from(crc32fast::hash(code)) | (code.len() as u64) << 32;
    Ok(match fingerprint {
        DELTA_PROGRAM => Some(FilterKind::Delta),
        E8_PROGRAM => Some(FilterKind::E8),
        E8E9_PROGRAM => Some(FilterKind::E8E9),
        RGB_PROGRAM => Some(FilterKind::Rgb),
        AUDIO_PROGRAM => Some(FilterKind::Audio),
        _ => None,
    })
}

pub(crate) struct FilterBits<'a> {
    bytes: &'a [u8],
    bit: usize,
}

impl<'a> FilterBits<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        FilterBits { bytes, bit: 0 }
    }

    pub(crate) fn bits(&mut self, count: u32) -> Result<u32, &'static str> {
        if self.bit + count as usize > self.bytes.len() * 8 {
            return Err("RAR 4 filter record is shorter than its fields");
        }
        let mut value = 0u64;
        for _ in 0..count {
            let byte = self.bytes[self.bit / 8];
            value = value << 1 | u64::from(byte >> (7 - self.bit % 8) & 1);
            self.bit += 1;
        }
        Ok(value as u32)
    }

    pub(crate) fn number(&mut self) -> Result<u32, &'static str> {
        Ok(match self.bits(2)? {
            0 => self.bits(4)?,
            1 => {
                let value = self.bits(8)?;
                if value >= 16 {
                    value
                } else {
                    0xFFFF_FF00 | value << 4 | self.bits(4)?
                }
            }
            2 => self.bits(16)?,
            _ => self.bits(32)?,
        })
    }
}

pub(crate) fn run_filter(
    kind: FilterKind,
    data: &[u8],
    registers: &[u32; 7],
    file_position: u64,
) -> Result<Vec<u8>, &'static str> {
    let length = data.len();
    let mut out = vec![0u8; length];
    match kind {
        FilterKind::Delta => {
            let channels = registers[0] as usize;
            if length > VM_WORK / 2 || !(1..=128).contains(&channels) {
                return Err("RAR 4 delta filter is outside its limits");
            }
            let mut source = data.iter();
            for channel in 0..channels {
                let mut last = 0u8;
                for at in (channel..length).step_by(channels) {
                    last = last.wrapping_sub(*source.next().unwrap_or(&0));
                    out[at] = last;
                }
            }
        }
        FilterKind::E8 | FilterKind::E8E9 => {
            if length > VM_WORK || length <= 4 {
                return Err("RAR 4 E8 filter is outside its limits");
            }
            out.copy_from_slice(data);
            let both = kind == FilterKind::E8E9;
            let mut at = 0;
            while at <= length - 5 {
                if out[at] == 0xE8 || (both && out[at] == 0xE9) {
                    let position = (file_position as u32).wrapping_add(at as u32 + 1);
                    let field: &mut [u8; 4] = (&mut out[at + 1..at + 5]).try_into().unwrap();
                    let address = u32::from_le_bytes(*field);
                    if (address as i32) < 0 {
                        if position >= address.wrapping_neg() {
                            *field = address.wrapping_add(E8_FILE_SIZE).to_le_bytes();
                        }
                    } else if address < E8_FILE_SIZE {
                        *field = address.wrapping_sub(position).to_le_bytes();
                    }
                    at += 4;
                }
                at += 1;
            }
        }
        FilterKind::Rgb => {
            let stride = registers[0] as usize;
            let byte_offset = registers[1] as usize;
            if length > VM_WORK / 2 || stride > length || length < 3 || byte_offset > 2 {
                return Err("RAR 4 RGB filter is outside its limits");
            }
            let mut source = data.iter();
            for channel in 0..3 {
                let mut byte = 0u8;
                let mut at = channel;
                while at < length {
                    if at >= stride {
                        let above = out[at - stride];
                        let above_right = out.get(at + 3 - stride).copied().unwrap_or(0);
                        let first = i32::from(above_right) - i32::from(above);
                        let second = i32::from(byte) - i32::from(above);
                        let both = first + second;
                        let (d1, d2, d3) = (first.abs(), second.abs(), both.abs());
                        if d1 > d2 || d1 > d3 {
                            byte = if d2 <= d3 { above_right } else { above };
                        }
                    }
                    byte = byte.wrapping_sub(*source.next().unwrap_or(&0));
                    out[at] = byte;
                    at += 3;
                }
            }
            let mut at = byte_offset;
            while at + 2 < length {
                out[at] = out[at].wrapping_add(out[at + 1]);
                out[at + 2] = out[at + 2].wrapping_add(out[at + 1]);
                at += 3;
            }
        }
        FilterKind::Audio => {
            let channels = registers[0] as usize;
            if length > VM_WORK / 2 || !(1..=128).contains(&channels) {
                return Err("RAR 4 audio filter is outside its limits");
            }
            let mut source = data.iter();
            for channel in 0..channels {
                let mut state = Audio::default();
                for at in (channel..length).step_by(channels) {
                    out[at] = state.next(*source.next().unwrap_or(&0) as i8);
                }
            }
        }
    }
    Ok(out)
}

#[derive(Default)]
struct Audio {
    weight: [i32; 3],
    delta: [i32; 3],
    last_delta: i32,
    error: [u32; 7],
    count: u32,
    last_byte: u8,
}

impl Audio {
    fn next(&mut self, delta: i8) -> u8 {
        self.delta[2] = self.delta[1];
        self.delta[1] = self.last_delta - self.delta[0];
        self.delta[0] = self.last_delta;
        let predicted = ((8 * i32::from(self.last_byte)
            + self.weight[0] * self.delta[0]
            + self.weight[1] * self.delta[1]
            + self.weight[2] * self.delta[2])
            >> 3)
            & 0xFF;
        let byte = (predicted as u8).wrapping_sub(delta as u8);
        let error = i32::from(delta) * 8;
        self.error[0] += error.unsigned_abs();
        for (index, delta) in self.delta.iter().enumerate() {
            self.error[1 + 2 * index] += (error - delta).unsigned_abs();
            self.error[2 + 2 * index] += (error + delta).unsigned_abs();
        }
        self.last_delta = i32::from(byte.wrapping_sub(self.last_byte) as i8);
        self.last_byte = byte;
        if self.count & 0x1F == 0 {
            let mut best = 0;
            for index in 1..7 {
                if self.error[index] < self.error[best] {
                    best = index;
                }
            }
            self.error = [0; 7];
            if best > 0 {
                let weight = &mut self.weight[(best - 1) / 2];
                if best % 2 == 1 {
                    if *weight >= -16 {
                        *weight -= 1;
                    }
                } else if *weight < 16 {
                    *weight += 1;
                }
            }
        }
        self.count = self.count.wrapping_add(1);
        byte
    }
}

type Ppmd<R> = ppmd_rust::Ppmd7aDecoder<PpmdInput<R>>;

pub(crate) struct Decoder<R: Read> {
    input: Rc<RefCell<Input<R>>>,
    window: Vec<u8>,
    written: u64,
    flushed: u64,
    size: u64,
    lengths: [u8; ALL_LENGTHS],
    tables: Option<Tables>,
    last_offset: u32,
    last_length: u32,
    old_offsets: [u32; 4],
    last_low_offset: u32,
    low_offset_repeats: u32,
    filters: VecDeque<Filter>,
    programs: Vec<Program>,
    last_program: usize,
    last_filter_end: u64,
    filtered: Vec<u8>,
    filtered_read: usize,
    ppmd: Option<Ppmd<R>>,
    model_bytes: u64,
    in_ppmd: bool,
    escape: u8,
    new_table: bool,
    ended: bool,
}

impl<R: Read> Decoder<R> {
    pub(crate) fn new(packed: R, packed_size: u64, size: u64) -> Self {
        Decoder {
            input: Rc::new(RefCell::new(Input::new(packed, packed_size))),
            window: Vec::new(),
            written: 0,
            flushed: 0,
            size,
            lengths: [0; ALL_LENGTHS],
            tables: None,
            last_offset: 0,
            last_length: 0,
            old_offsets: [0; 4],
            last_low_offset: 0,
            low_offset_repeats: 0,
            filters: VecDeque::new(),
            programs: Vec::new(),
            last_program: 0,
            last_filter_end: 0,
            filtered: Vec::new(),
            filtered_read: 0,
            ppmd: None,
            model_bytes: 0,
            in_ppmd: false,
            escape: DEFAULT_ESCAPE,
            new_table: true,
            ended: false,
        }
    }

    fn grow_for(&mut self, end: u64) -> io::Result<()> {
        let have = self.window.len() as u64;
        if end <= have || have == WINDOW {
            return Ok(());
        }
        let wanted = end.max(have * 2).min(WINDOW);
        self.window
            .try_reserve_exact((wanted - have) as usize)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "RAR 4 window does not fit in memory",
                )
            })?;
        self.window.resize(wanted as usize, 0);
        Ok(())
    }

    fn put(&mut self, byte: u8) -> io::Result<()> {
        if self.written >= self.size {
            return Err(invalid("RAR 4 stream decodes past the declared size"));
        }
        self.grow_for(self.written + 1)?;
        let at = (self.written % WINDOW) as usize;
        self.window[at] = byte;
        self.written += 1;
        Ok(())
    }

    fn copy(&mut self, distance: u32, length: u32) -> io::Result<()> {
        let (distance, length) = (u64::from(distance), u64::from(length));
        if distance == 0 || distance > self.written || distance > WINDOW {
            return Err(invalid("RAR 4 match reaches before the start of its file"));
        }
        if length > self.size - self.written {
            return Err(invalid("RAR 4 stream decodes past the declared size"));
        }
        self.grow_for(self.written + length)?;
        for step in 0..length {
            let byte = self.window[((self.written - distance + step) % WINDOW) as usize];
            self.window[((self.written + step) % WINDOW) as usize] = byte;
        }
        self.written += length;
        Ok(())
    }

    fn read_codes(&mut self) -> io::Result<()> {
        let mut input = self.input.borrow_mut();
        input.align();
        if input.bits(1)? == 1 {
            drop(input);
            return self.start_ppmd();
        }
        self.in_ppmd = false;
        self.last_low_offset = 0;
        self.low_offset_repeats = 0;
        if input.bits(1)? == 0 {
            self.lengths = [0; ALL_LENGTHS];
        }
        let mut bit_lengths = [0u8; BIT_LENGTH_SYMBOLS];
        let mut filled = 0;
        while filled < BIT_LENGTH_SYMBOLS {
            let value = input.bits(4)? as u8;
            if value == 15 {
                let zeros = input.bits(4)? as usize;
                if zeros > 0 {
                    filled = (filled + zeros + 2).min(BIT_LENGTH_SYMBOLS);
                    continue;
                }
            }
            bit_lengths[filled] = value;
            filled += 1;
        }
        let code = Huffman::new(&bit_lengths).map_err(invalid)?;
        let mut index = 0;
        while index < ALL_LENGTHS {
            let symbol = input.symbol(&code)?;
            if symbol < 16 {
                self.lengths[index] = (self.lengths[index] + symbol as u8) & 0xF;
                index += 1;
                continue;
            }
            let repeat = match symbol {
                16 | 18 => 3 + input.bits(3)? as usize,
                _ => 11 + input.bits(7)? as usize,
            };
            let value = if symbol < 18 {
                if index == 0 {
                    return Err(invalid("RAR 4 table repeats a length before the first"));
                }
                self.lengths[index - 1]
            } else {
                0
            };
            let end = (index + repeat).min(ALL_LENGTHS);
            self.lengths[index..end].fill(value);
            index = end;
        }
        let (main, rest) = self.lengths.split_at(MAIN_SYMBOLS);
        let (offset, rest) = rest.split_at(OFFSET_SYMBOLS);
        let (low_offset, length) = rest.split_at(LOW_OFFSET_SYMBOLS);
        self.tables = Some(Tables {
            main: Huffman::new(main).map_err(invalid)?,
            offset: Huffman::new(offset).map_err(invalid)?,
            low_offset: Huffman::new(low_offset).map_err(invalid)?,
            length: Huffman::new(length).map_err(invalid)?,
        });
        self.new_table = false;
        Ok(())
    }

    fn start_ppmd(&mut self) -> io::Result<()> {
        let mut input = self.input.borrow_mut();
        let flags = input.bits(7)?;
        let memory = if flags & 0x20 != 0 {
            Some((input.bits(8)? + 1) << 20)
        } else {
            None
        };
        self.escape = if flags & 0x40 != 0 {
            input.bits(8)? as u8
        } else {
            DEFAULT_ESCAPE
        };
        drop(input);
        let Some(memory) = memory else {
            return Err(invalid(
                "RAR 4 PPMd block continues the model before it, which Zipper does not read",
            ));
        };
        let mut order = (flags & 0x1F) + 1;
        if order > 16 {
            order = 16 + (order - 16) * 3;
        }
        if order < 2 || memory > LARGEST_PPMD_MEMORY {
            return Err(invalid("RAR 4 PPMd model is outside its limits"));
        }
        self.model_bytes += u64::from(memory);
        if self.model_bytes > PPMD_MODEL_ALLOWANCE + PPMD_MODEL_BYTES_PER_OUTPUT * self.written {
            return Err(invalid(
                "RAR 4 PPMd models restart more often than their output justifies",
            ));
        }
        self.ppmd = None;
        let decoder =
            ppmd_rust::Ppmd7aDecoder::new(PpmdInput(Rc::clone(&self.input)), order, memory)
                .map_err(|_| invalid("RAR 4 PPMd model cannot start"))?;
        self.ppmd = Some(decoder);
        self.in_ppmd = true;
        self.new_table = false;
        Ok(())
    }

    fn ppmd_symbol(&mut self) -> io::Result<u8> {
        let ppmd = self
            .ppmd
            .as_mut()
            .ok_or_else(|| invalid("RAR 4 PPMd block has no model"))?;
        let mut byte = [0u8];
        match ppmd.read(&mut byte)? {
            1 => Ok(byte[0]),
            _ => Err(invalid("RAR 4 PPMd data ends early")),
        }
    }

    fn decode_ppmd(&mut self) -> io::Result<()> {
        let symbol = self.ppmd_symbol()?;
        if symbol != self.escape {
            return self.put(symbol);
        }
        match self.ppmd_symbol()? {
            0 => {
                self.new_table = true;
                Ok(())
            }
            2 => {
                self.ended = true;
                Ok(())
            }
            3 => Err(invalid(
                "RAR 4 PPMd block holds a filter, which Zipper does not read",
            )),
            4 => {
                let mut offset = 0u32;
                for _ in 0..3 {
                    offset = offset << 8 | u32::from(self.ppmd_symbol()?);
                }
                let length = u32::from(self.ppmd_symbol()?);
                self.copy(offset + 2, length + 32)
            }
            5 => {
                let length = u32::from(self.ppmd_symbol()?);
                self.copy(1, length + 4)
            }
            _ => self.put(symbol),
        }
    }

    fn read_filter(&mut self) -> io::Result<()> {
        let mut input = self.input.borrow_mut();
        let flags = input.bits(8)?;
        let length = match (flags & 7) + 1 {
            7 => input.bits(8)? + 7,
            8 => input.bits(16)?,
            other => other,
        } as usize;
        let mut record = vec![0u8; length];
        for byte in &mut record {
            *byte = input.bits(8)? as u8;
        }
        drop(input);
        self.add_filter(flags, &record).map_err(invalid)
    }

    fn add_filter(&mut self, flags: u32, record: &[u8]) -> Result<(), &'static str> {
        let mut bits = FilterBits::new(record);
        let number = if flags & 0x80 != 0 {
            let number = bits.number()? as usize;
            if number == 0 {
                self.filters.clear();
                self.programs.clear();
                self.last_filter_end = 0;
                0
            } else {
                number - 1
            }
        } else {
            self.last_program
        };
        if number > self.programs.len() || number >= MOST_PROGRAMS {
            return Err("RAR 4 filter names a program that does not exist");
        }
        self.last_program = number;
        let mut start = u64::from(bits.number()?) + self.written;
        if flags & 0x40 != 0 {
            start += 258;
        }
        let length = if flags & 0x20 != 0 {
            u64::from(bits.number()?)
        } else {
            self.programs
                .get(number)
                .map_or(0, |program| program.old_length)
        };
        let mut registers = [0u32; 7];
        registers[4] = length as u32;
        if flags & 0x10 != 0 {
            let mask = bits.bits(7)?;
            for (index, register) in registers.iter_mut().enumerate() {
                if mask & (1 << index) != 0 {
                    *register = bits.number()?;
                }
            }
        }
        if number == self.programs.len() {
            let size = u64::from(bits.number()?);
            if size == 0 || size > LONGEST_PROGRAM {
                return Err("RAR 4 filter program is empty or too long");
            }
            let mut code = vec![0u8; size as usize];
            for byte in &mut code {
                *byte = bits.bits(8)? as u8;
            }
            self.programs.push(Program {
                kind: program_kind(&code)?,
                old_length: 0,
            });
        }
        self.programs[number].old_length = length;
        if flags & 0x08 != 0 {
            let size = u64::from(bits.number()?);
            if size > VM_USER_GLOBAL {
                return Err("RAR 4 filter global data is too long");
            }
            for _ in 0..size {
                bits.bits(8)?;
            }
        }
        let kind = self.programs[number]
            .kind
            .ok_or("RAR 4 filter runs a program that is not a standard filter")?;
        if length as usize > VM_MEMORY
            || u64::from(registers[4]) != length
            || start < self.last_filter_end
            || start + length > self.size
            || self.filters.len() >= MOST_FILTERS
        {
            return Err("RAR 4 filter is outside its file or the filter memory");
        }
        if length == 0 {
            return Ok(());
        }
        self.last_filter_end = start + length;
        self.filters.push_back(Filter {
            kind,
            start,
            length,
            registers,
        });
        Ok(())
    }

    fn decode_lz(&mut self) -> io::Result<()> {
        let tables = self
            .tables
            .take()
            .ok_or_else(|| invalid("RAR 4 block has no tables"))?;
        let result = self.lz_symbol(&tables);
        if self.tables.is_none() {
            self.tables = Some(tables);
        }
        result
    }

    fn lz_symbol(&mut self, tables: &Tables) -> io::Result<()> {
        let symbol = self.input.borrow_mut().symbol(&tables.main)?;
        let (offset, length) = match symbol {
            0..=255 => return self.put(symbol as u8),
            256 => {
                let mut input = self.input.borrow_mut();
                if input.bits(1)? == 0 {
                    input.bits(1)?;
                    self.ended = true;
                } else {
                    self.new_table = true;
                }
                return Ok(());
            }
            257 => return self.read_filter(),
            258 => {
                if self.last_length == 0 {
                    return Ok(());
                }
                (self.last_offset, self.last_length)
            }
            259..=262 => {
                let index = usize::from(symbol - 259);
                let offset = self.old_offsets[index];
                let mut input = self.input.borrow_mut();
                let slot = usize::from(input.symbol(&tables.length)?);
                let length = 2 + LENGTH_BASES[slot] + input.bits(LENGTH_BITS[slot])?;
                self.old_offsets.copy_within(0..index, 1);
                self.old_offsets[0] = offset;
                (offset, length)
            }
            263..=270 => {
                let slot = usize::from(symbol - 263);
                let offset =
                    SHORT_BASES[slot] + 1 + self.input.borrow_mut().bits(SHORT_BITS[slot])?;
                self.old_offsets.copy_within(0..3, 1);
                self.old_offsets[0] = offset;
                (offset, 2)
            }
            _ => {
                let slot = usize::from(symbol - 271);
                let mut input = self.input.borrow_mut();
                let mut length = 3 + LENGTH_BASES[slot] + input.bits(LENGTH_BITS[slot])?;
                let slot = usize::from(input.symbol(&tables.offset)?);
                let extra = OFFSET_BITS[slot];
                let mut offset = OFFSET_BASES[slot] + 1;
                if slot > 9 {
                    if extra > 4 {
                        offset += input.bits(extra - 4)? << 4;
                    }
                    if self.low_offset_repeats > 0 {
                        self.low_offset_repeats -= 1;
                        offset += self.last_low_offset;
                    } else {
                        let low = input.symbol(&tables.low_offset)?;
                        if low == LOW_OFFSET_REPEAT {
                            self.low_offset_repeats = LOW_OFFSET_REPEATS;
                            offset += self.last_low_offset;
                        } else {
                            offset += u32::from(low);
                            self.last_low_offset = u32::from(low);
                        }
                    }
                } else {
                    offset += input.bits(extra)?;
                }
                if offset >= 0x4_0000 {
                    length += 1;
                }
                if offset >= 0x2000 {
                    length += 1;
                }
                self.old_offsets.copy_within(0..3, 1);
                self.old_offsets[0] = offset;
                (offset, length)
            }
        };
        self.last_offset = offset;
        self.last_length = length;
        self.copy(offset, length)
    }

    fn decode_some(&mut self) -> io::Result<()> {
        while !self.ended && self.written < self.size && self.written - self.flushed < BATCH {
            if self
                .filters
                .front()
                .is_some_and(|filter| self.written >= filter.start + filter.length)
            {
                return Ok(());
            }
            if self.new_table {
                self.read_codes()?;
            } else if self.in_ppmd {
                self.decode_ppmd()?;
            } else {
                self.decode_lz()?;
            }
        }
        Ok(())
    }

    fn ready_end(&self) -> u64 {
        match self.filters.front() {
            Some(filter) => self.written.min(filter.start),
            None => self.written,
        }
    }

    fn run_ready_filter(&mut self) -> io::Result<bool> {
        let Some(&filter) = self.filters.front() else {
            return Ok(false);
        };
        if self.flushed != filter.start || self.written < filter.start + filter.length {
            return Ok(false);
        }
        self.filters.pop_front();
        let data: Vec<u8> = (0..filter.length)
            .map(|step| self.window[((filter.start + step) % WINDOW) as usize])
            .collect();
        self.filtered =
            run_filter(filter.kind, &data, &filter.registers, filter.start).map_err(invalid)?;
        self.filtered_read = 0;
        self.flushed += filter.length;
        Ok(true)
    }

    pub(crate) fn finish_at_declared_size(&self) -> io::Result<()> {
        if self.flushed != self.size
            || self.filtered_read < self.filtered.len()
            || !self.filters.is_empty()
        {
            return Err(invalid("RAR 4 stream ends before its declared size"));
        }
        Ok(())
    }
}

impl<R: Read> Read for Decoder<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut copied = 0;
        while copied < output.len() {
            if self.filtered_read < self.filtered.len() {
                let run = (self.filtered.len() - self.filtered_read).min(output.len() - copied);
                output[copied..copied + run]
                    .copy_from_slice(&self.filtered[self.filtered_read..self.filtered_read + run]);
                self.filtered_read += run;
                copied += run;
                continue;
            }
            let end = self.ready_end();
            if self.flushed < end {
                let at = self.flushed % WINDOW;
                let run = (end - self.flushed)
                    .min(WINDOW - at)
                    .min((output.len() - copied) as u64) as usize;
                let at = at as usize;
                output[copied..copied + run].copy_from_slice(&self.window[at..at + run]);
                copied += run;
                self.flushed += run as u64;
                continue;
            }
            if self.run_ready_filter()? {
                continue;
            }
            if copied > 0 || self.flushed == self.size {
                break;
            }
            if self.ended || self.written >= self.size && self.filters.is_empty() {
                return Err(invalid("RAR 4 stream ends before its declared size"));
            }
            let before = (self.written, self.input.borrow().bit, self.filters.len());
            self.decode_some()?;
            if (self.written, self.input.borrow().bit, self.filters.len()) == before
                && !self.new_table
            {
                return Err(invalid("RAR 4 stream makes no progress"));
            }
        }
        Ok(copied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_numbers_read_each_width_and_the_negative_short_form() {
        let mut bits = FilterBits::new(&[0b0010_1001, 0b0000_0011, 0b0100_0000]);
        assert_eq!(bits.number().unwrap(), 0b1010);
        assert_eq!(bits.number().unwrap(), 0xFFFF_FF00 | 3 << 4 | 4);
        let mut short = FilterBits::new(&[0b0100_0000]);
        assert!(short.number().is_err());
    }

    #[test]
    fn delta_filters_undo_a_per_channel_difference() {
        let original: Vec<u8> = (0..64u32).map(|i| (i * 7 + i * i) as u8).collect();
        for channels in 1..=4usize {
            let mut encoded = Vec::new();
            for channel in 0..channels {
                let mut last = 0u8;
                for at in (channel..original.len()).step_by(channels) {
                    encoded.push(last.wrapping_sub(original[at]));
                    last = original[at];
                }
            }
            let mut registers = [0; 7];
            registers[0] = channels as u32;
            assert_eq!(
                run_filter(FilterKind::Delta, &encoded, &registers, 0).unwrap(),
                original
            );
        }
    }

    #[test]
    fn e8_filters_turn_absolute_calls_back_into_relative_ones() {
        let data = [0x90, 0xE8, 0x10, 0, 0, 0, 0x90, 0x90];
        let out = run_filter(FilterKind::E8, &data, &[0; 7], 100).unwrap();
        assert_eq!(&out[2..6], &(0x10u32.wrapping_sub(102)).to_le_bytes());
        assert!(run_filter(FilterKind::E8, &data[..4], &[0; 7], 0).is_err());
    }

    #[test]
    fn programs_are_known_by_their_check_and_fingerprint() {
        assert!(program_kind(&[1, 2]).is_err());
        assert_eq!(program_kind(&[3, 1, 2]).unwrap(), None);
        assert!(program_kind(&[]).is_err());
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(34)]
    fn vm_numbers_never_panic_and_read_only_their_record() {
        let bytes: [u8; 5] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let mut bits = FilterBits::new(&bytes[..length]);
        if bits.number().is_ok() {
            assert!(bits.bit <= length * 8);
        }
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    const MAIN_BITS: u32 = 9;
    const OFFSET_BITS_WIDTH: u32 = 6;
    const LOW_OFFSET_BITS: u32 = 5;
    const LENGTH_BITS_WIDTH: u32 = 5;

    enum Token {
        Literal(u8),
        Match { length: u32, offset: u32 },
        End,
    }

    #[derive(Default)]
    struct Writer {
        bytes: Vec<u8>,
        bits: usize,
    }

    impl Writer {
        fn put(&mut self, value: u32, count: u32) {
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

    fn slot(bases: &[u32], bits: &[u32], value: u32) -> usize {
        (0..bases.len())
            .rev()
            .find(|&slot| value >= bases[slot] && value - bases[slot] < 1 << bits[slot])
            .unwrap()
    }

    fn stream_with_fixed_width_tables(tokens: &[Token]) -> Vec<u8> {
        let mut writer = Writer::default();
        writer.put(0, 2);
        for _ in 0..BIT_LENGTH_SYMBOLS {
            writer.put(5, 4);
        }
        for (count, width) in [
            (MAIN_SYMBOLS, MAIN_BITS),
            (OFFSET_SYMBOLS, OFFSET_BITS_WIDTH),
            (LOW_OFFSET_SYMBOLS, LOW_OFFSET_BITS),
            (LENGTH_SYMBOLS, LENGTH_BITS_WIDTH),
        ] {
            for _ in 0..count {
                writer.put(width, 5);
            }
        }
        for token in tokens {
            match *token {
                Token::Literal(byte) => writer.put(u32::from(byte), MAIN_BITS),
                Token::Match { length, offset } => {
                    let bonus = u32::from(offset >= 0x2000) + u32::from(offset >= 0x4_0000);
                    let length = length - 3 - bonus;
                    let length_slot = slot(&LENGTH_BASES, &LENGTH_BITS, length);
                    writer.put(271 + length_slot as u32, MAIN_BITS);
                    writer.put(length - LENGTH_BASES[length_slot], LENGTH_BITS[length_slot]);
                    let value = offset - 1;
                    let offset_slot = slot(&OFFSET_BASES, &OFFSET_BITS, value);
                    writer.put(offset_slot as u32, OFFSET_BITS_WIDTH);
                    let extra = value - OFFSET_BASES[offset_slot];
                    let bits = OFFSET_BITS[offset_slot];
                    if offset_slot > 9 {
                        if bits > 4 {
                            writer.put(extra >> 4, bits - 4);
                        }
                        writer.put(extra & 15, LOW_OFFSET_BITS);
                    } else {
                        writer.put(extra, bits);
                    }
                }
                Token::End => {
                    writer.put(256, MAIN_BITS);
                    writer.put(0, 2);
                }
            }
        }
        writer.bytes
    }

    fn decode(packed: &[u8], size: u64) -> io::Result<Vec<u8>> {
        let mut decoder = Decoder::new(packed, packed.len() as u64, size);
        let mut out = Vec::new();
        let mut buffer = [0u8; 5];
        while (out.len() as u64) < size {
            let read = decoder.read(&mut buffer)?;
            if read == 0 {
                return Err(invalid("short"));
            }
            out.extend_from_slice(&buffer[..read]);
        }
        decoder.finish_at_declared_size()?;
        Ok(out)
    }

    #[test]
    fn literals_and_matches_decode_as_a_byte_at_a_time_model() {
        let mut tokens: Vec<Token> = (0..40u8).map(Token::Literal).collect();
        tokens.push(Token::Match {
            length: 9,
            offset: 3,
        });
        tokens.push(Token::Match {
            length: 30,
            offset: 40,
        });
        tokens.push(Token::End);
        let mut model: Vec<u8> = (0..40u8).collect();
        for (length, offset) in [(9, 3), (30, 40)] {
            for _ in 0..length {
                model.push(model[model.len() - offset]);
            }
        }
        let packed = stream_with_fixed_width_tables(&tokens);
        assert_eq!(decode(&packed, model.len() as u64).unwrap(), model);
    }

    #[test]
    fn a_match_before_the_start_or_past_the_declared_size_is_refused() {
        let early = stream_with_fixed_width_tables(&[
            Token::Literal(1),
            Token::Match {
                length: 4,
                offset: 2,
            },
        ]);
        assert!(decode(&early, 5).is_err());
        let long = stream_with_fixed_width_tables(&[
            Token::Literal(1),
            Token::Match {
                length: 4,
                offset: 1,
            },
        ]);
        assert_eq!(decode(&long, 5).unwrap(), [1; 5]);
        assert!(decode(&long, 3).is_err());
    }

    #[test]
    fn a_stream_that_ends_before_its_size_or_is_cut_is_refused() {
        let packed =
            stream_with_fixed_width_tables(&[Token::Literal(1), Token::Literal(2), Token::End]);
        assert_eq!(decode(&packed, 2).unwrap(), [1, 2]);
        assert!(decode(&packed, 3).is_err());
        let literals_end = (2 + 4 * 20 + 5 * ALL_LENGTHS + 2 * MAIN_BITS as usize) / 8;
        for cut in 0..literals_end {
            assert!(decode(&packed[..cut], 2).is_err(), "cut to {cut}");
        }
    }
}
