#![allow(dead_code)]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Literal(u8),
    Match { length: usize, distance: usize },
}

pub struct Random(pub u64);

impl Random {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit.max(1) as u64) as usize
    }
}

pub fn parse(data: &[u8], window: usize, shortest: usize, longest: usize) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut heads: std::collections::HashMap<[u8; 3], Vec<usize>> = Default::default();
    let mut at = 0;
    while at < data.len() {
        let mut best = (0, 0);
        if at + 3 <= data.len() {
            let key = [data[at], data[at + 1], data[at + 2]];
            if let Some(starts) = heads.get(&key) {
                for &start in starts.iter().rev().take(16) {
                    let distance = at - start - 1;
                    if distance >= window {
                        break;
                    }
                    let mut length = 0;
                    while length < longest
                        && at + length < data.len()
                        && data[start + length] == data[at + length]
                    {
                        length += 1;
                    }
                    if length > best.0 {
                        best = (length, distance);
                    }
                }
            }
        }
        let step = if best.0 >= shortest {
            tokens.push(Token::Match {
                length: best.0,
                distance: best.1,
            });
            best.0
        } else {
            tokens.push(Token::Literal(data[at]));
            1
        };
        for position in at..at + step {
            if position + 3 <= data.len() {
                heads
                    .entry([data[position], data[position + 1], data[position + 2]])
                    .or_default()
                    .push(position);
            }
        }
        at += step;
    }
    tokens
}

pub fn replay(tokens: &[Token]) -> Vec<u8> {
    let mut out = Vec::new();
    for token in tokens {
        match *token {
            Token::Literal(byte) => out.push(byte),
            Token::Match { length, distance } => {
                for _ in 0..length {
                    out.push(out[out.len() - distance - 1]);
                }
            }
        }
    }
    out
}

#[derive(Default)]
pub struct Bits {
    pub bytes: Vec<u8>,
    value: u64,
    count: u32,
}

impl Bits {
    pub fn put(&mut self, value: u64, count: u32) {
        for bit in (0..count).rev() {
            self.value = self.value << 1 | (value >> bit & 1);
            self.count += 1;
            if self.count == 8 {
                self.bytes.push(self.value as u8);
                self.value = 0;
                self.count = 0;
            }
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            let pad = 8 - self.count;
            self.put(0, pad);
        }
        self.bytes
    }
}

pub fn lengths(weights: &[u64], longest: u8) -> Vec<u8> {
    let mut weights = weights.to_vec();
    loop {
        let mut nodes: Vec<(u64, Vec<usize>)> = weights
            .iter()
            .enumerate()
            .filter(|(_, weight)| **weight > 0)
            .map(|(index, &weight)| (weight, vec![index]))
            .collect();
        let mut lengths = vec![0u8; weights.len()];
        if nodes.len() == 1 {
            lengths[nodes[0].1[0]] = 1;
            return lengths;
        }
        while nodes.len() > 1 {
            nodes.sort_by(|a, b| b.0.cmp(&a.0).then(b.1[0].cmp(&a.1[0])));
            let (wa, a) = nodes.pop().unwrap();
            let (wb, b) = nodes.pop().unwrap();
            for &leaf in a.iter().chain(&b) {
                lengths[leaf] += 1;
            }
            nodes.push((wa + wb, [a, b].concat()));
        }
        if lengths.iter().all(|&length| length <= longest) {
            return lengths;
        }
        for weight in &mut weights {
            if *weight > 0 {
                *weight = weight.div_ceil(2);
            }
        }
    }
}

pub fn codes(lengths: &[u8]) -> Vec<u32> {
    let mut codes = vec![0u32; lengths.len()];
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

fn slot(distance: usize) -> usize {
    (usize::BITS - distance.leading_zeros()) as usize
}

fn put_short_length(bits: &mut Bits, length: u8) {
    if length < 7 {
        bits.put(u64::from(length), 3);
    } else {
        bits.put(7, 3);
        for _ in 7..length {
            bits.put(1, 1);
        }
        bits.put(0, 1);
    }
}

fn used(lengths: &[u8]) -> usize {
    lengths
        .iter()
        .rposition(|&length| length != 0)
        .map_or(0, |last| last + 1)
}

fn put_short_table(bits: &mut Bits, lengths: &[u8], count_bits: u32, skip_after: Option<usize>) {
    let count = used(lengths);
    bits.put(count as u64, count_bits);
    let mut index = 0;
    while index < count {
        put_short_length(bits, lengths[index]);
        index += 1;
        if Some(index) == skip_after {
            let zeros = lengths[index..count.min(index + 3)]
                .iter()
                .take_while(|&&l| l == 0)
                .count();
            bits.put(zeros as u64, 2);
            index += zeros;
        }
    }
}

fn zero_runs(lengths: &[u8]) -> Vec<(usize, u32, u32)> {
    let mut codes = Vec::new();
    let count = used(lengths);
    let mut index = 0;
    while index < count {
        if lengths[index] != 0 {
            codes.push((usize::from(lengths[index]) + 2, 0, 0));
            index += 1;
            continue;
        }
        let run = lengths[index..count]
            .iter()
            .take_while(|&&l| l == 0)
            .count();
        match run {
            1 | 2 => codes.extend(std::iter::repeat_n((0, 0, 0), run)),
            3..=18 => codes.push((1, (run - 3) as u32, 4)),
            19 => {
                codes.push((0, 0, 0));
                codes.push((1, 15, 4));
            }
            _ => codes.push((2, (run - 20) as u32, 9)),
        }
        index += run;
    }
    codes
}

#[derive(Clone, Copy, Debug)]
pub struct Shape {
    pub single_when_possible: bool,
}

pub fn encode(
    tokens: &[Token],
    position_count_bits: u32,
    blocks: &[usize],
    shape: Shape,
) -> Vec<u8> {
    let mut bits = Bits::default();
    let mut rest = tokens;
    let mut sizes = blocks.iter().copied();
    while !rest.is_empty() {
        let take = sizes
            .next()
            .unwrap_or(rest.len())
            .clamp(1, 0xFFFF)
            .min(rest.len());
        let (block, after) = rest.split_at(take);
        encode_block(&mut bits, block, position_count_bits, shape);
        rest = after;
    }
    bits.finish()
}

fn encode_block(bits: &mut Bits, block: &[Token], position_count_bits: u32, shape: Shape) {
    let mut character_weights = vec![0u64; 510];
    let mut position_weights = vec![0u64; 26];
    for token in block {
        match *token {
            Token::Literal(byte) => character_weights[usize::from(byte)] += 1,
            Token::Match { length, distance } => {
                character_weights[256 + length - 3] += 1;
                position_weights[slot(distance)] += 1;
            }
        }
    }
    bits.put(block.len() as u64, 16);
    let distinct = |weights: &[u64]| weights.iter().filter(|&&w| w > 0).count();
    let single_characters = shape.single_when_possible && distinct(&character_weights) == 1;
    let character_lengths = lengths(&character_weights, 16);
    let runs = zero_runs(&character_lengths);
    if single_characters {
        bits.put(0, 5);
        bits.put(0, 5);
        let symbol = character_weights.iter().position(|&w| w > 0).unwrap();
        bits.put(0, 9);
        bits.put(symbol as u64, 9);
    } else {
        let mut tree_weights = vec![0u64; 19];
        for &(code, _, _) in &runs {
            tree_weights[code] += 1;
        }
        if shape.single_when_possible && distinct(&tree_weights) == 1 {
            let symbol = tree_weights.iter().position(|&w| w > 0).unwrap();
            bits.put(0, 5);
            bits.put(symbol as u64, 5);
            bits.put(used(&character_lengths) as u64, 9);
            for &(_, extra, extra_bits) in &runs {
                bits.put(u64::from(extra), extra_bits);
            }
        } else {
            let tree_lengths = lengths(&tree_weights, 16);
            let tree_codes = codes(&tree_lengths);
            put_short_table(bits, &tree_lengths, 5, Some(3));
            bits.put(used(&character_lengths) as u64, 9);
            for &(code, extra, extra_bits) in &runs {
                bits.put(u64::from(tree_codes[code]), u32::from(tree_lengths[code]));
                bits.put(u64::from(extra), extra_bits);
            }
        }
    }
    let matches = distinct(&position_weights);
    let position_lengths = if matches == 0 || (shape.single_when_possible && matches == 1) {
        let symbol = position_weights.iter().position(|&w| w > 0).unwrap_or(0);
        bits.put(0, position_count_bits);
        bits.put(symbol as u64, position_count_bits);
        None
    } else {
        let position_lengths = lengths(&position_weights, 16);
        put_short_table(bits, &position_lengths, position_count_bits, None);
        Some(position_lengths)
    };
    let character_codes = codes(&character_lengths);
    let position_codes = position_lengths.as_deref().map(codes);
    for token in block {
        let (symbol, distance) = match *token {
            Token::Literal(byte) => (usize::from(byte), None),
            Token::Match { length, distance } => (256 + length - 3, Some(distance)),
        };
        if !single_characters {
            bits.put(
                u64::from(character_codes[symbol]),
                u32::from(character_lengths[symbol]),
            );
        }
        if let Some(distance) = distance {
            let slot = slot(distance);
            if let (Some(lengths), Some(codes)) = (&position_lengths, &position_codes) {
                bits.put(u64::from(codes[slot]), u32::from(lengths[slot]));
            }
            if slot > 1 {
                bits.put((distance - (1 << (slot - 1))) as u64, slot as u32 - 1);
            }
        }
    }
}

pub fn encode_fastest(tokens: &[Token]) -> Vec<u8> {
    let mut bits = Bits::default();
    for token in tokens {
        match *token {
            Token::Literal(byte) => bits.put(u64::from(byte), 9),
            Token::Match { length, distance } => {
                let value = length - 1;
                let width = (usize::BITS - 1 - value.leading_zeros()) as usize;
                bits.put(1, 1);
                for _ in 1..width {
                    bits.put(1, 1);
                }
                if width < 7 {
                    bits.put(0, 1);
                }
                bits.put((value - (1 << width)) as u64, width as u32);
                let ones = (9..13)
                    .take_while(|&w| distance >= (1 << (w + 1)) - 512)
                    .count();
                let width = 9 + ones;
                for _ in 0..ones {
                    bits.put(1, 1);
                }
                if width < 13 {
                    bits.put(0, 1);
                }
                bits.put((distance + 512 - (1 << width)) as u64, width as u32);
            }
        }
    }
    bits.finish()
}

pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in bytes {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}
