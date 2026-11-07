#![allow(dead_code)]

pub const CHUNK: usize = 32 * 1024;
const MOST_SLOTS: usize = 50;
const MAIN_SYMBOLS: usize = 256 + MOST_SLOTS * 8;
const LENGTH_SYMBOLS: usize = 249;
const LONGEST_MATCH: usize = 257;

#[derive(Clone, Debug)]
pub struct Chunk {
    pub bytes: Vec<u8>,
    pub size: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Verbatim,
    Aligned,
    Uncompressed,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub window_bits: u8,
    pub translation: Option<u32>,
    pub kinds: Vec<Kind>,
    pub longest_block: usize,
    pub empty_blocks: bool,
    pub padding_in_next_chunk: bool,
    pub runs: bool,
    pub plain_aligned: bool,
    pub literal_odds: u32,
    pub reset_interval: Option<u32>,
    pub encode_resets: bool,
    pub seed: u64,
    pub plan: Vec<(Kind, usize)>,
}

impl Options {
    pub fn plain(window_bits: u8) -> Self {
        Options {
            window_bits,
            translation: None,
            kinds: vec![Kind::Verbatim],
            longest_block: 1 << 20,
            empty_blocks: false,
            padding_in_next_chunk: false,
            runs: true,
            plain_aligned: false,
            literal_odds: 0,
            reset_interval: None,
            encode_resets: true,
            seed: 1,
            plan: Vec::new(),
        }
    }

    pub fn random(next: &mut impl FnMut() -> u64, window_bits: u8) -> Self {
        let mut pick = |limit: u64| next() % limit;
        let kinds = match pick(5) {
            0 => vec![Kind::Verbatim],
            1 => vec![Kind::Aligned],
            2 => vec![Kind::Uncompressed],
            3 => vec![Kind::Verbatim, Kind::Uncompressed],
            _ => vec![Kind::Verbatim, Kind::Aligned, Kind::Uncompressed],
        };
        Options {
            window_bits,
            translation: match pick(4) {
                0 => None,
                1 => Some(1 + pick(5000) as u32),
                2 => Some(12_000_000),
                _ => Some(1 + pick(1 << 31) as u32),
            },
            kinds,
            longest_block: [1, 7, 2000, CHUNK - 1, CHUNK + 3, 1 << 20][pick(6) as usize],
            empty_blocks: pick(6) == 0,
            padding_in_next_chunk: pick(2) == 0,
            runs: pick(4) != 0,
            plain_aligned: pick(3) == 0,
            literal_odds: [0, 2, 8][pick(3) as usize],
            reset_interval: if pick(5) == 0 {
                Some(1 + pick(3) as u32)
            } else {
                None
            },
            encode_resets: true,
            seed: next() | 1,
            plan: Vec::new(),
        }
    }
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
        (self.next() % limit.max(1) as u64) as usize
    }
}

const fn extra_bits(slot: usize) -> u32 {
    if slot < 4 {
        0
    } else if slot < 36 {
        (slot as u32 - 2) / 2
    } else {
        17
    }
}

fn position_base(slot: usize) -> u32 {
    (0..slot).map(|slot| 1u32 << extra_bits(slot)).sum()
}

fn position_slots(window_bits: u8) -> usize {
    match window_bits {
        20 => 42,
        21 => 50,
        bits => 2 * bits as usize,
    }
}

fn decode_e8(value: u32, position: u32, translation: u32) -> u32 {
    if value < translation {
        value.wrapping_sub(position)
    } else if value > !position {
        value.wrapping_add(translation)
    } else {
        value
    }
}

fn forward_e8(data: &mut [u8], processed: u32, translation: u32) {
    if data.len() <= 10 {
        return;
    }
    let limit = data.len() - 10;
    let mut index = 0;
    while index < limit {
        if data[index] != 0xE8 {
            index += 1;
            continue;
        }
        let position = processed.wrapping_add(index as u32);
        let wanted = u32::from_le_bytes(data[index + 1..index + 5].try_into().unwrap());
        let stored = [
            wanted.wrapping_add(position),
            wanted.wrapping_sub(translation),
            wanted,
        ]
        .into_iter()
        .find(|&stored| decode_e8(stored, position, translation) == wanted)
        .expect("every value has an E8 spelling");
        data[index + 1..index + 5].copy_from_slice(&stored.to_le_bytes());
        index += 5;
    }
}

pub fn huffman_lengths(frequencies: &[u32], limit: u8) -> Vec<u8> {
    let mut weights: Vec<u64> = frequencies.iter().map(|&f| u64::from(f)).collect();
    let used = weights.iter().filter(|&&w| w > 0).count();
    if used == 0 {
        return vec![0; frequencies.len()];
    }
    if used == 1 {
        let other = usize::from(weights[0] != 0);
        weights[other] = 1;
    }
    loop {
        let mut heap = std::collections::BinaryHeap::new();
        let mut parents: Vec<usize> = Vec::new();
        let mut leaves = vec![usize::MAX; weights.len()];
        for (symbol, &weight) in weights.iter().enumerate().filter(|(_, w)| **w > 0) {
            leaves[symbol] = parents.len();
            parents.push(usize::MAX);
            heap.push(std::cmp::Reverse((weight, parents.len() - 1)));
        }
        while heap.len() > 1 {
            let std::cmp::Reverse((first, a)) = heap.pop().unwrap();
            let std::cmp::Reverse((second, b)) = heap.pop().unwrap();
            parents.push(usize::MAX);
            let node = parents.len() - 1;
            parents[a] = node;
            parents[b] = node;
            heap.push(std::cmp::Reverse((first + second, node)));
        }
        let depths: Vec<u8> = leaves
            .iter()
            .map(|&leaf| {
                let mut depth = 0u8;
                let mut node = leaf;
                while node != usize::MAX && parents[node] != usize::MAX {
                    node = parents[node];
                    depth += 1;
                }
                depth
            })
            .collect();
        if depths.iter().all(|&d| d <= limit) {
            return depths;
        }
        for weight in weights.iter_mut().filter(|w| **w > 0) {
            *weight = (*weight >> 1) | 1;
        }
    }
}

fn canonical_codes(lengths: &[u8]) -> Vec<u32> {
    let mut codes = vec![0; lengths.len()];
    let mut code = 0u32;
    for length in 1..=16u8 {
        for (symbol, &l) in lengths.iter().enumerate() {
            if l == length {
                codes[symbol] = code;
                code += 1;
            }
        }
        code <<= 1;
    }
    codes
}

pub struct Writer {
    chunks: Vec<Chunk>,
    current: Vec<u8>,
    pending: u32,
    pub pending_bits: u32,
}

impl Writer {
    pub fn new() -> Self {
        Writer {
            chunks: Vec::new(),
            current: Vec::new(),
            pending: 0,
            pending_bits: 0,
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.align();
        self.current
    }

    pub fn bits(&mut self, value: u32, count: u32) {
        for bit in (0..count).rev() {
            self.pending = (self.pending << 1) | ((value >> bit) & 1);
            self.pending_bits += 1;
            if self.pending_bits == 16 {
                self.current
                    .extend_from_slice(&(self.pending as u16).to_le_bytes());
                self.pending = 0;
                self.pending_bits = 0;
            }
        }
    }

    pub fn align(&mut self) {
        if self.pending_bits > 0 {
            self.bits(0, 16 - self.pending_bits);
        }
    }

    pub fn raw(&mut self, bytes: &[u8]) {
        assert_eq!(self.pending_bits, 0);
        self.current.extend_from_slice(bytes);
    }

    fn reach(&mut self, position: usize) {
        if position == (self.chunks.len() + 1) * CHUNK {
            self.end_chunk(CHUNK);
        }
    }

    fn end_chunk(&mut self, size: usize) {
        self.align();
        self.chunks.push(Chunk {
            bytes: std::mem::take(&mut self.current),
            size,
        });
    }
}

#[derive(Clone, Copy)]
pub enum Token {
    Literal(u8),
    Match { length: usize, distance: usize },
}

struct Code {
    lengths: Vec<u8>,
    codes: Vec<u32>,
}

impl Code {
    fn new(lengths: Vec<u8>) -> Self {
        let codes = canonical_codes(&lengths);
        Code { lengths, codes }
    }

    fn write(&self, writer: &mut Writer, symbol: usize) {
        assert!(self.lengths[symbol] > 0, "symbol {symbol} has no code");
        writer.bits(self.codes[symbol], u32::from(self.lengths[symbol]));
    }
}

fn write_lengths(
    writer: &mut Writer,
    random: &mut Random,
    runs: bool,
    previous: &mut [u8],
    lengths: &[u8],
) {
    let mut symbols: Vec<(usize, u32, u32)> = Vec::new();
    let mut index = 0;
    while index < lengths.len() {
        let value = lengths[index];
        let same = lengths[index..].iter().take_while(|&&l| l == value).count();
        let choice = if runs { random.below(3) } else { 3 };
        if value == 0 && same >= 20 && choice != 0 {
            let count = same.min(51);
            symbols.push((18, (count - 20) as u32, 5));
            index += count;
        } else if value == 0 && same >= 4 && choice != 0 {
            let count = same.min(19);
            symbols.push((17, (count - 4) as u32, 4));
            index += count;
        } else if same >= 4 && choice == 1 {
            let count = same.min(5);
            let delta = (usize::from(previous[index]) + 17 - usize::from(value)) % 17;
            symbols.push((19, (count - 4) as u32, 1));
            symbols.push((delta, 0, 0));
            index += count;
        } else {
            let delta = (usize::from(previous[index]) + 17 - usize::from(value)) % 17;
            symbols.push((delta, 0, 0));
            index += 1;
        }
    }
    previous.copy_from_slice(lengths);
    let mut frequencies = [0u32; 20];
    for &(symbol, _, _) in &symbols {
        frequencies[symbol] += 1;
    }
    let pretree = Code::new(huffman_lengths(&frequencies, 15));
    for &length in &pretree.lengths {
        writer.bits(u32::from(length), 4);
    }
    for &(symbol, extra, extra_count) in &symbols {
        pretree.write(writer, symbol);
        writer.bits(extra, extra_count);
    }
}

struct Encoder<'a> {
    data: &'a [u8],
    options: &'a Options,
    random: Random,
    writer: Writer,
    repeats: [usize; 3],
    previous_main: Vec<u8>,
    previous_length: Vec<u8>,
    slots: usize,
    window: usize,
    stream_start: usize,
    chains: std::collections::HashMap<u32, Vec<usize>>,
}

impl Encoder<'_> {
    fn chunk_end(&self, position: usize) -> usize {
        (position / CHUNK + 1) * CHUNK
    }

    fn longest_match(&self, position: usize, limit: usize, distance: usize) -> usize {
        if distance == 0 || distance > position - self.stream_start || distance > self.window - 3 {
            return 0;
        }
        (0..limit.min(LONGEST_MATCH))
            .take_while(|&k| self.data[position + k] == self.data[position + k - distance])
            .count()
    }

    fn tokens(&mut self, start: usize, end: usize) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut repeats = self.repeats;
        let mut position = start;
        while position < end {
            let limit = end.min(self.chunk_end(position)) - position;
            let mut best = (0, 0);
            for &distance in &repeats {
                let length = self.longest_match(position, limit, distance);
                if length > best.0 {
                    best = (length, distance);
                }
            }
            if position + 3 <= self.data.len() {
                let key = u32::from_le_bytes([
                    self.data[position],
                    self.data[position + 1],
                    self.data[position + 2],
                    0,
                ]);
                if let Some(chain) = self.chains.get(&key) {
                    for &earlier in chain.iter().rev().take(8) {
                        let length = self.longest_match(position, limit, position - earlier);
                        if length > best.0 {
                            best = (length, position - earlier);
                        }
                    }
                }
            }
            let odds = self.options.literal_odds;
            let literal = best.0 < 2 || (odds > 0 && self.random.below(odds as usize) == 0);
            let length = if literal {
                tokens.push(Token::Literal(self.data[position]));
                1
            } else {
                let length = if self.random.below(4) == 0 {
                    2 + self.random.below(best.0 - 1)
                } else {
                    best.0
                };
                let distance = best.1;
                tokens.push(Token::Match { length, distance });
                match repeats.iter().position(|&r| r == distance) {
                    Some(0) => {}
                    Some(slot) => repeats.swap(0, slot),
                    None => repeats = [distance, repeats[0], repeats[1]],
                }
                length
            };
            for at in position..position + length {
                if at + 3 <= self.data.len() {
                    let key = u32::from_le_bytes([
                        self.data[at],
                        self.data[at + 1],
                        self.data[at + 2],
                        0,
                    ]);
                    self.chains.entry(key).or_default().push(at);
                }
            }
            position += length;
        }
        tokens
    }

    fn slot_of(&mut self, distance: usize) -> (usize, u32) {
        if let Some(slot) = self.repeats.iter().position(|&r| r == distance) {
            if slot != 0 {
                self.repeats.swap(0, slot);
            }
            return (slot, 0);
        }
        self.repeats = [distance, self.repeats[0], self.repeats[1]];
        let formatted = distance as u32 + 2;
        let slot = (3..self.slots)
            .rfind(|&slot| position_base(slot) <= formatted)
            .unwrap();
        (slot, formatted - position_base(slot))
    }

    fn compressed_block(&mut self, kind: Kind, start: usize, end: usize, output: &mut usize) {
        let tokens = self.tokens(start, end);
        let saved = self.repeats;
        let mut main_frequencies = vec![0u32; 256 + self.slots * 8];
        let mut length_frequencies = vec![0u32; LENGTH_SYMBOLS];
        let mut aligned_frequencies = [0u32; 8];
        let mut coded = Vec::new();
        for &token in &tokens {
            match token {
                Token::Literal(byte) => {
                    main_frequencies[usize::from(byte)] += 1;
                    coded.push((usize::from(byte), None, 0, 0));
                }
                Token::Match { length, distance } => {
                    let (slot, footer) = self.slot_of(distance);
                    let header = (length - 2).min(7);
                    let symbol = 256 + slot * 8 + header;
                    main_frequencies[symbol] += 1;
                    let long = (header == 7).then(|| length - 9);
                    if let Some(long) = long {
                        length_frequencies[long] += 1;
                    }
                    let extra = extra_bits(slot);
                    if kind == Kind::Aligned && extra >= 3 {
                        aligned_frequencies[(footer & 7) as usize] += 1;
                    }
                    coded.push((symbol, long, footer, extra));
                }
            }
        }
        self.repeats = saved;
        let used = main_frequencies.iter().filter(|&&f| f > 0).count();
        if used < 2 {
            main_frequencies[0] += 1;
            main_frequencies[1] += 1;
        }
        let writer = &mut self.writer;
        writer.bits(if kind == Kind::Aligned { 2 } else { 1 }, 3);
        let length = (end - start) as u32;
        writer.bits(length >> 8, 16);
        writer.bits(length & 0xFF, 8);
        let aligned = if kind == Kind::Aligned {
            let lengths = if self.options.plain_aligned {
                vec![3; 8]
            } else {
                let mut frequencies = aligned_frequencies;
                frequencies[self.random.below(8)] += 1;
                huffman_lengths(&frequencies, 7)
            };
            for &l in &lengths {
                writer.bits(u32::from(l), 3);
            }
            Some(Code::new(lengths))
        } else {
            None
        };
        let main = Code::new(huffman_lengths(&main_frequencies, 16));
        let runs = self.options.runs;
        let (literals, matches) = self.previous_main.split_at_mut(256);
        write_lengths(
            writer,
            &mut self.random,
            runs,
            literals,
            &main.lengths[..256],
        );
        write_lengths(
            writer,
            &mut self.random,
            runs,
            &mut matches[..self.slots * 8],
            &main.lengths[256..],
        );
        let long = Code::new(huffman_lengths(&length_frequencies, 16));
        write_lengths(
            writer,
            &mut self.random,
            runs,
            &mut self.previous_length,
            &long.lengths,
        );
        for (token, (symbol, long_length, footer, extra)) in tokens.iter().zip(coded) {
            self.writer.reach(*output);
            main.write(&mut self.writer, symbol);
            if let Some(long_length) = long_length {
                long.write(&mut self.writer, long_length);
            }
            match &aligned {
                Some(aligned) if extra >= 3 => {
                    self.writer.bits(footer >> 3, extra - 3);
                    aligned.write(&mut self.writer, (footer & 7) as usize);
                }
                _ => self.writer.bits(footer, extra),
            }
            if let Token::Match { distance, .. } = token {
                self.slot_of(*distance);
            }
            *output += match token {
                Token::Literal(_) => 1,
                Token::Match { length, .. } => *length,
            };
        }
    }

    fn uncompressed_block(&mut self, start: usize, end: usize, padding_in_next_chunk: bool) {
        let length = (end - start) as u32;
        self.writer.bits(3, 3);
        self.writer.bits(length >> 8, 16);
        self.writer.bits(length & 0xFF, 8);
        let pad = 16 - self.writer.pending_bits;
        self.writer.bits(0, pad);
        for repeat in self.repeats {
            self.writer.raw(&(repeat as u32).to_le_bytes());
        }
        let mut position = start;
        while position < end {
            self.writer.reach(position);
            let next = end.min(self.chunk_end(position));
            self.writer.raw(&self.data[position..next]);
            position = next;
        }
        for at in start..end {
            if at + 3 <= self.data.len() {
                let key =
                    u32::from_le_bytes([self.data[at], self.data[at + 1], self.data[at + 2], 0]);
                self.chains.entry(key).or_default().push(at);
            }
        }
        if length % 2 == 1 {
            if padding_in_next_chunk {
                self.writer.reach(end);
            }
            self.writer.raw(&[0]);
        }
    }

    fn reset(&mut self, position: usize) {
        self.repeats = [1; 3];
        self.previous_main = vec![0; MAIN_SYMBOLS];
        self.previous_length = vec![0; LENGTH_SYMBOLS];
        self.stream_start = position;
        match self.options.translation {
            Some(size) => {
                self.writer.bits(1, 1);
                self.writer.bits(size >> 16, 16);
                self.writer.bits(size & 0xFFFF, 16);
            }
            None => self.writer.bits(0, 1),
        }
    }
}

pub fn encode(original: &[u8], options: &Options) -> Vec<Chunk> {
    assert!(!original.is_empty());
    let reset_every = options
        .reset_interval
        .filter(|_| options.encode_resets)
        .map(|interval| interval as usize * CHUNK);
    let mut data = original.to_vec();
    if let Some(translation) = options.translation {
        let mut processed = 0u32;
        for (index, chunk) in data.chunks_mut(CHUNK).enumerate() {
            if reset_every.is_some_and(|every| (index * CHUNK).is_multiple_of(every)) {
                processed = 0;
            }
            if processed < 1 << 30 {
                forward_e8(chunk, processed, translation);
                processed += chunk.len() as u32;
            }
        }
    }
    let mut encoder = Encoder {
        data: &data,
        options,
        random: Random(options.seed),
        writer: Writer::new(),
        repeats: [1; 3],
        previous_main: vec![0; MAIN_SYMBOLS],
        previous_length: vec![0; LENGTH_SYMBOLS],
        slots: position_slots(options.window_bits),
        window: 1 << options.window_bits,
        stream_start: 0,
        chains: Default::default(),
    };
    encoder.reset(0);
    let mut output = 0;
    let mut planned = options.plan.iter();
    while output < data.len() {
        let next_reset = reset_every.map_or(data.len(), |every| (output / every + 1) * every);
        let room = data.len().min(next_reset) - output;
        let empty = options.empty_blocks && encoder.random.below(8) == 0;
        let length = if empty {
            0
        } else {
            1 + encoder.random.below(options.longest_block.min(room))
        };
        let mut kind = options.kinds[encoder.random.below(options.kinds.len())];
        let mut length = length;
        if let Some(&(planned_kind, planned_length)) = planned.next() {
            kind = planned_kind;
            length = planned_length.min(room);
        }
        encoder.writer.reach(output);
        if reset_every.is_some_and(|every| output % every == 0) && encoder.stream_start != output {
            encoder.reset(output);
        }
        let end = output + length;
        if kind == Kind::Uncompressed {
            let reset_follows = reset_every.is_some_and(|every| end % every == 0);
            let padding_in_next_chunk = options.padding_in_next_chunk
                && end % CHUNK == 0
                && end < data.len()
                && !reset_follows;
            encoder.uncompressed_block(output, end, padding_in_next_chunk);
        } else {
            let mut position = output;
            encoder.compressed_block(kind, output, end, &mut position);
        }
        output = end;
    }
    let size = match data.len() % CHUNK {
        0 => CHUNK,
        rest => rest,
    };
    encoder.writer.end_chunk(size);
    encoder.writer.chunks
}

pub fn verbatim_chunk_of_exact_tokens(
    window_bits: u8,
    block_length: usize,
    tokens: &[Token],
) -> Vec<u8> {
    let slots = position_slots(window_bits);
    let mut repeats = [1usize; 3];
    let mut coded = Vec::new();
    let mut main_frequencies = vec![0u32; 256 + slots * 8];
    let mut length_frequencies = vec![0u32; LENGTH_SYMBOLS];
    for &token in tokens {
        match token {
            Token::Literal(byte) => {
                main_frequencies[usize::from(byte)] += 1;
                coded.push((usize::from(byte), None, 0, 0));
            }
            Token::Match { length, distance } => {
                let (slot, footer) = match repeats.iter().position(|&r| r == distance) {
                    Some(slot) => {
                        repeats.swap(0, slot);
                        (slot, 0)
                    }
                    None => {
                        repeats = [distance, repeats[0], repeats[1]];
                        let formatted = distance as u32 + 2;
                        let slot = (3..slots)
                            .rfind(|&slot| position_base(slot) <= formatted)
                            .unwrap();
                        (slot, formatted - position_base(slot))
                    }
                };
                let header = (length - 2).min(7);
                let symbol = 256 + slot * 8 + header;
                main_frequencies[symbol] += 1;
                let long = (header == 7).then(|| length - 9);
                if let Some(long) = long {
                    length_frequencies[long] += 1;
                }
                coded.push((symbol, long, footer, extra_bits(slot)));
            }
        }
    }
    main_frequencies[0] += 1;
    main_frequencies[1] += 1;
    let mut writer = Writer::new();
    let mut random = Random(1);
    writer.bits(0, 1);
    writer.bits(1, 3);
    writer.bits(block_length as u32 >> 8, 16);
    writer.bits(block_length as u32 & 0xFF, 8);
    let main = Code::new(huffman_lengths(&main_frequencies, 16));
    let mut previous = vec![0u8; 256 + slots * 8];
    let (literals, matches) = previous.split_at_mut(256);
    write_lengths(
        &mut writer,
        &mut random,
        false,
        literals,
        &main.lengths[..256],
    );
    write_lengths(
        &mut writer,
        &mut random,
        false,
        matches,
        &main.lengths[256..],
    );
    let long = Code::new(huffman_lengths(&length_frequencies, 16));
    let mut previous_length = vec![0u8; LENGTH_SYMBOLS];
    write_lengths(
        &mut writer,
        &mut random,
        false,
        &mut previous_length,
        &long.lengths,
    );
    for (symbol, long_length, footer, extra) in coded {
        main.write(&mut writer, symbol);
        if let Some(long_length) = long_length {
            long.write(&mut writer, long_length);
        }
        writer.bits(footer, extra);
    }
    writer.finish()
}
