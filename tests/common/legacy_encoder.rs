//! Test encoders for ZIP's Shrink and Implode methods, which no current tool
//! writes, and a ZIP writer for their entries. Their output is checked
//! against Info-ZIP `unzip` and 7-Zip before Zipper's reader is trusted.
#![allow(dead_code)]
use std::collections::HashMap;

pub const SHRINK: u16 = 1;
pub const IMPLODE: u16 = 6;
pub const EIGHT_K_WINDOW: u16 = 1 << 1;
pub const LITERAL_TREE: u16 = 1 << 2;

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, limit: u64) -> u64 {
        self.next() % limit
    }
}

pub struct Entry {
    pub name: String,
    pub method: u16,
    pub flags: u16,
    pub crc: u32,
    pub size: u32,
    pub packed: Vec<u8>,
}

impl Entry {
    pub fn new(name: &str, method: u16, flags: u16, data: &[u8], packed: Vec<u8>) -> Self {
        Entry {
            name: name.to_owned(),
            method,
            flags,
            crc: crc32fast::hash(data),
            size: data.len() as u32,
            packed,
        }
    }
}

pub fn zip(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    let header = |out: &mut Vec<u8>, entry: &Entry, central: Option<u32>| {
        match central {
            Some(_) => {
                out.extend_from_slice(b"PK\x01\x02");
                out.extend_from_slice(&20u16.to_le_bytes());
            }
            None => out.extend_from_slice(b"PK\x03\x04"),
        }
        out.extend_from_slice(&10u16.to_le_bytes());
        out.extend_from_slice(&entry.flags.to_le_bytes());
        out.extend_from_slice(&entry.method.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0x5021u16.to_le_bytes());
        out.extend_from_slice(&entry.crc.to_le_bytes());
        out.extend_from_slice(&(entry.packed.len() as u32).to_le_bytes());
        out.extend_from_slice(&entry.size.to_le_bytes());
        out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        if let Some(offset) = central {
            out.extend_from_slice(&[0; 10]);
            out.extend_from_slice(&offset.to_le_bytes());
        }
        out.extend_from_slice(entry.name.as_bytes());
    };
    for entry in entries {
        offsets.push(out.len() as u32);
        header(&mut out, entry, None);
        out.extend_from_slice(&entry.packed);
    }
    let central = out.len() as u32;
    for (entry, &offset) in entries.iter().zip(&offsets) {
        header(&mut out, entry, Some(offset));
    }
    let central_size = out.len() as u32 - central;
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[derive(Default)]
pub struct Bits {
    out: Vec<u8>,
    held: u64,
    count: u32,
}

impl Bits {
    pub fn put(&mut self, value: u32, bits: u32) {
        self.held |= u64::from(value) << self.count;
        self.count += bits;
        while self.count >= 8 {
            self.out.push(self.held as u8);
            self.held >>= 8;
            self.count -= 8;
        }
    }

    pub fn put_high_first(&mut self, value: u32, bits: u32) {
        for bit in (0..bits).rev() {
            self.put(value >> bit & 1, 1);
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.out.push(self.held as u8);
        }
        self.out
    }
}

pub fn shrink_codes(codes: &[(u32, u32)]) -> Vec<u8> {
    let mut bits = Bits::default();
    for &(code, width) in codes {
        bits.put(code, width);
    }
    bits.finish()
}

const CODES: usize = 8192;

struct ShrinkTable {
    prefix: Vec<u16>,
    suffix: Vec<u8>,
    used: Vec<bool>,
    usable: Vec<bool>,
    next_free: usize,
    known: HashMap<(u16, u8), u16>,
}

impl ShrinkTable {
    fn new() -> Self {
        let mut used = vec![false; CODES];
        used[..256].fill(true);
        ShrinkTable {
            prefix: vec![0; CODES],
            suffix: vec![0; CODES],
            used: used.clone(),
            usable: used,
            next_free: 257,
            known: HashMap::new(),
        }
    }

    fn full(&self) -> bool {
        !self.used[self.next_free.min(CODES - 1)..].contains(&false)
    }

    fn add(&mut self, previous: u16, byte: u8) {
        while self.used[self.next_free] {
            self.next_free += 1;
        }
        let slot = self.next_free;
        self.prefix[slot] = previous;
        self.suffix[slot] = byte;
        self.used[slot] = true;
        self.next_free += 1;
        let usable = usize::from(previous) != slot
            && self.usable[usize::from(previous)]
            && self.used[usize::from(previous)]
            && !self.known.contains_key(&(previous, byte));
        self.usable[slot] = usable;
        if usable {
            self.known.insert((previous, byte), slot as u16);
        }
    }

    fn partial_clear(&mut self) {
        let mut parent = vec![false; CODES];
        for code in 257..CODES {
            if self.used[code] {
                parent[usize::from(self.prefix[code])] = true;
            }
        }
        for code in (257..CODES).filter(|&code| !parent[code]) {
            if self.used[code] {
                self.used[code] = false;
                if self.usable[code] {
                    self.known.remove(&(self.prefix[code], self.suffix[code]));
                }
                self.usable[code] = false;
            }
        }
        self.next_free = 257;
    }
}

/// Shrinks `data` greedily. `clear_every` emits a partial clear after that
/// many codes, and the encoder always clears before its table fills.
pub fn shrink(data: &[u8], clear_every: Option<usize>) -> Vec<u8> {
    shrink_codes(&shrink_trace(data, clear_every))
}

pub fn shrink_trace(data: &[u8], clear_every: Option<usize>) -> Vec<(u32, u32)> {
    let mut bits: Vec<(u32, u32)> = Vec::new();
    let mut width = 9;
    let mut table = ShrinkTable::new();
    let mut previous: Option<u16> = None;
    let mut position = 0;
    let mut emitted = 0usize;
    while position < data.len() {
        if let Some(previous) = previous {
            if clear_every.is_some_and(|every| emitted.is_multiple_of(every)) || table.full() {
                bits.push((256, width));
                bits.push((2, width));
                table.partial_clear();
                assert!(!table.full(), "a partial clear freed nothing");
            }
            table.add(previous, data[position]);
        }
        let mut code = u16::from(data[position]);
        position += 1;
        while let Some(&longer) = data
            .get(position)
            .and_then(|&byte| table.known.get(&(code, byte)))
        {
            code = longer;
            position += 1;
        }
        while u32::from(code) >= 1 << width {
            bits.push((256, width));
            bits.push((1, width));
            width += 1;
        }
        bits.push((u32::from(code), width));
        emitted += 1;
        previous = Some(code);
    }
    bits
}

/// A random Shrink code stream and the bytes 7-Zip's rules give it. When
/// `valid` is false, the last code names a cleared or cyclic entry, or the
/// table is full, and `data` holds the bytes before that code.
pub struct RandomShrink {
    pub codes: Vec<(u32, u32)>,
    pub data: Vec<u8>,
    pub valid: bool,
}

struct Reference {
    prefix: Vec<usize>,
    suffix: Vec<u8>,
    used: Vec<bool>,
    lowest_free: usize,
}

impl Reference {
    fn string(&self, code: usize) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let mut walk = code;
        while walk > 256 {
            if !self.used[walk] || out.len() > CODES {
                return None;
            }
            out.push(self.suffix[walk]);
            walk = self.prefix[walk];
        }
        if walk == 256 {
            return None;
        }
        out.push(walk as u8);
        out.reverse();
        Some(out)
    }

    fn clear(&mut self) {
        let mut parent = vec![false; CODES];
        for code in 257..CODES {
            if self.used[code] {
                parent[self.prefix[code]] = true;
            }
        }
        for (used, &has_child) in self.used[257..].iter_mut().zip(&parent[257..]) {
            *used &= has_child;
        }
        self.lowest_free = 257;
    }
}

pub fn random_shrink(rng: &mut Rng, steps: usize) -> RandomShrink {
    let mut table = Reference {
        prefix: vec![0; CODES],
        suffix: vec![0; CODES],
        used: (0..CODES).map(|code| code < 256).collect(),
        lowest_free: 257,
    };
    let mut codes = Vec::new();
    let mut data = Vec::new();
    let mut width = 9u32;
    let mut previous: Option<(usize, u8)> = None;
    for _ in 0..steps {
        let roll = rng.below(100);
        if previous.is_some() && roll < 6 {
            codes.push((256, width));
            codes.push((2, width));
            table.clear();
            continue;
        }
        if previous.is_some() && roll < 9 && width < 13 {
            codes.push((256, width));
            codes.push((1, width));
            width += 1;
            continue;
        }
        let mut added = None;
        if let Some((code, first)) = previous {
            while table.lowest_free < CODES && table.used[table.lowest_free] {
                table.lowest_free += 1;
            }
            if table.lowest_free == CODES {
                codes.push((rng.below(256) as u32, width));
                return RandomShrink {
                    codes,
                    data,
                    valid: false,
                };
            }
            let slot = table.lowest_free;
            table.prefix[slot] = code;
            table.suffix[slot] = first;
            table.used[slot] = true;
            added = Some(slot);
        }
        let entries: Vec<usize> = (257..1 << width).filter(|&c| table.used[c]).collect();
        let code = match roll {
            _ if previous.is_none() || roll < 35 => rng.below(256) as usize,
            _ if roll < 50 && added.is_some() => added.unwrap(),
            _ if roll < 97 && !entries.is_empty() => {
                let mut pick = || entries[rng.below(entries.len() as u64) as usize];
                let mut code = pick();
                for _ in 0..4 {
                    if table.string(code).is_some() {
                        break;
                    }
                    code = pick();
                }
                code
            }
            _ => 257 + rng.below((1 << width) - 257) as usize,
        };
        let Some(string) = table.string(code) else {
            codes.push((code as u32, width));
            return RandomShrink {
                codes,
                data,
                valid: false,
            };
        };
        if let Some(slot) = added {
            table.suffix[slot] = string[0];
        }
        codes.push((code as u32, width));
        data.extend_from_slice(&string);
        previous = Some((code, string[0]));
    }
    RandomShrink {
        codes,
        data,
        valid: true,
    }
}

pub fn random_tokens(rng: &mut Rng, flags: u16, count: usize) -> (Vec<Token>, Vec<u8>) {
    let window = if flags & EIGHT_K_WINDOW != 0 {
        8192
    } else {
        4096
    };
    let shortest = if flags & LITERAL_TREE != 0 { 3 } else { 2 };
    let mut tokens = Vec::new();
    let mut data: Vec<u8> = Vec::new();
    for _ in 0..count {
        if rng.below(2) == 0 {
            let byte = rng.below(256) as u8;
            tokens.push(Token::Literal(byte));
            data.push(byte);
            continue;
        }
        let distance = 1 + rng.below(window as u64) as usize;
        let length = shortest
            + match rng.below(4) {
                0 => 63 + rng.below(256) as usize,
                _ => rng.below(63) as usize,
            };
        for _ in 0..length {
            let byte = data
                .len()
                .checked_sub(distance)
                .map_or(0, |from| data[from]);
            data.push(byte);
        }
        tokens.push(Token::Match { distance, length });
    }
    (tokens, data)
}

pub struct Code {
    pub lengths: Vec<u8>,
}

impl Code {
    pub fn flat(symbols: usize) -> Self {
        let bits = symbols.trailing_zeros() as u8;
        Code {
            lengths: vec![bits; symbols],
        }
    }

    pub fn random(symbols: usize, rng: &mut Rng) -> Self {
        let mut leaves = vec![0u8];
        while leaves.len() < symbols {
            let splittable: Vec<usize> = (0..leaves.len()).filter(|&i| leaves[i] < 16).collect();
            let pick = splittable[rng.below(splittable.len() as u64) as usize];
            let depth = leaves[pick] + 1;
            leaves[pick] = depth;
            leaves.push(depth);
        }
        for index in (1..leaves.len()).rev() {
            leaves.swap(index, rng.below(index as u64 + 1) as usize);
        }
        Code { lengths: leaves }
    }

    pub fn description(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut index = 0;
        while index < self.lengths.len() {
            let length = self.lengths[index];
            let mut run = 1;
            while run < 16 && self.lengths.get(index + run) == Some(&length) {
                run += 1;
            }
            out.push(((run as u8 - 1) << 4) | (length - 1));
            index += run;
        }
        let mut stored = vec![(out.len() - 1) as u8];
        stored.extend(out);
        stored
    }

    pub fn codes(&self) -> Vec<u32> {
        let mut order: Vec<usize> = (0..self.lengths.len()).collect();
        order.sort_by_key(|&symbol| (self.lengths[symbol], symbol));
        let mut codes = vec![0u32; self.lengths.len()];
        let (mut code, mut length) = (0u32, 0u8);
        for symbol in order {
            code <<= self.lengths[symbol] - length;
            length = self.lengths[symbol];
            codes[symbol] = !code & ((1 << length) - 1);
            code += 1;
        }
        codes
    }
}

pub struct ImplodeTrees {
    pub literals: Option<Code>,
    pub lengths: Code,
    pub distances: Code,
}

pub enum Token {
    Literal(u8),
    Match { distance: usize, length: usize },
}

pub fn parse(data: &[u8], flags: u16, rng: &mut Rng, literal_chance: u64) -> Vec<Token> {
    let window = if flags & EIGHT_K_WINDOW != 0 {
        8192
    } else {
        4096
    };
    let shortest = if flags & LITERAL_TREE != 0 { 3 } else { 2 };
    let longest = 63 + shortest + 255;
    let at = |index: isize| {
        usize::try_from(index)
            .ok()
            .and_then(|i| data.get(i))
            .copied()
            .unwrap_or(0)
    };
    let mut recent: HashMap<(u8, u8), Vec<usize>> = HashMap::new();
    let mut tokens = Vec::new();
    let mut position = 0;
    while position < data.len() {
        let mut best = (0, 0);
        let mut candidates: Vec<isize> = recent
            .get(&(data[position], at(position as isize + 1)))
            .map(|found| found.iter().rev().take(24).map(|&p| p as isize).collect())
            .unwrap_or_default();
        candidates.push(position as isize - window as isize);
        candidates.push(position as isize - 1 - rng.below(window as u64) as isize);
        for start in candidates {
            let distance = (position as isize - start) as usize;
            if distance == 0 || distance > window {
                continue;
            }
            let mut length = 0;
            while length < longest
                && position + length < data.len()
                && at(start + length as isize) == data[position + length]
            {
                length += 1;
            }
            if length > best.1 {
                best = (distance, length);
            }
        }
        let step = if best.1 >= shortest && rng.below(100) >= literal_chance {
            tokens.push(Token::Match {
                distance: best.0,
                length: best.1,
            });
            best.1
        } else {
            tokens.push(Token::Literal(data[position]));
            1
        };
        for index in position..position + step {
            if index + 1 < data.len() {
                recent
                    .entry((data[index], data[index + 1]))
                    .or_default()
                    .push(index);
            }
        }
        position += step;
    }
    tokens
}

pub fn implode_tokens(tokens: &[Token], flags: u16, trees: &ImplodeTrees) -> Vec<u8> {
    let low_bits = if flags & EIGHT_K_WINDOW != 0 { 7 } else { 6 };
    let shortest = if flags & LITERAL_TREE != 0 { 3 } else { 2 };
    let mut out = Vec::new();
    if flags & LITERAL_TREE != 0 {
        out.extend(trees.literals.as_ref().unwrap().description());
    }
    out.extend(trees.lengths.description());
    out.extend(trees.distances.description());
    let literal_codes = trees.literals.as_ref().map(Code::codes);
    let length_codes = trees.lengths.codes();
    let distance_codes = trees.distances.codes();
    let mut bits = Bits::default();
    let put_symbol = |bits: &mut Bits, code: &Code, codes: &[u32], symbol: usize| {
        bits.put_high_first(codes[symbol], u32::from(code.lengths[symbol]));
    };
    for token in tokens {
        match *token {
            Token::Literal(byte) => {
                bits.put(1, 1);
                match &literal_codes {
                    Some(codes) => put_symbol(
                        &mut bits,
                        trees.literals.as_ref().unwrap(),
                        codes,
                        usize::from(byte),
                    ),
                    None => bits.put(u32::from(byte), 8),
                }
            }
            Token::Match { distance, length } => {
                bits.put(0, 1);
                let stored = distance - 1;
                bits.put((stored & ((1 << low_bits) - 1)) as u32, low_bits);
                put_symbol(
                    &mut bits,
                    &trees.distances,
                    &distance_codes,
                    stored >> low_bits,
                );
                let length = length - shortest;
                put_symbol(&mut bits, &trees.lengths, &length_codes, length.min(63));
                if length >= 63 {
                    bits.put((length - 63) as u32, 8);
                }
            }
        }
    }
    out.extend(bits.finish());
    out
}

pub fn implode(data: &[u8], flags: u16, seed: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let tokens = parse(data, flags, &mut rng, 10);
    let flat = seed.is_multiple_of(3);
    let mut code = |symbols| {
        if flat {
            Code::flat(symbols)
        } else {
            Code::random(symbols, &mut rng)
        }
    };
    let trees = ImplodeTrees {
        literals: (flags & LITERAL_TREE != 0).then(|| code(256)),
        lengths: code(64),
        distances: code(64),
    };
    implode_tokens(&tokens, flags, &trees)
}
