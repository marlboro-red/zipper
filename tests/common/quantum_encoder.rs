#![allow(dead_code)]

pub const CHUNK: usize = 32 * 1024;
pub const LONGEST_MATCH: usize = 259;
pub const MODELS: usize = 9;
const LENGTH_EXTRA: [u32; 27] = [
    0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const SLOT_LIMITS: [usize; 3] = [24, 36, 42];

#[derive(Clone, Debug)]
pub struct Chunk {
    pub bytes: Vec<u8>,
    pub size: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Literal(u8),
    Match { length: usize, distance: usize },
}

impl Token {
    pub fn size(&self) -> usize {
        match self {
            Token::Literal(_) => 1,
            Token::Match { length, .. } => *length,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub window_bits: u8,
    pub seed: u64,
    pub literal_odds: u64,
    pub shorten_odds: u64,
    pub chain: usize,
}

impl Options {
    pub fn plain(window_bits: u8) -> Self {
        Options {
            window_bits,
            seed: 1,
            literal_odds: 0,
            shorten_odds: 0,
            chain: 32,
        }
    }

    pub fn random(next: &mut impl FnMut() -> u64, window_bits: u8) -> Self {
        let mut pick = |limit: u64| next() % limit;
        Options {
            window_bits,
            seed: pick(u64::MAX) | 1,
            literal_odds: [0, 0, 5, 50, 100][pick(5) as usize],
            shorten_odds: [0, 0, 20, 70][pick(4) as usize],
            chain: [1, 8, 32, 128][pick(4) as usize],
        }
    }
}

fn length_table() -> [usize; 28] {
    let mut base = [0usize; 28];
    base[0] = 5;
    for symbol in 0..27 {
        base[symbol + 1] = base[symbol] + (1 << LENGTH_EXTRA[symbol]);
    }
    base
}

fn slot_extra(slot: usize) -> u32 {
    if slot < 4 { 0 } else { (slot as u32 - 2) / 2 }
}

fn slot_table() -> [usize; 43] {
    let mut base = [0usize; 43];
    for slot in 0..42 {
        base[slot + 1] = base[slot] + (1 << slot_extra(slot));
    }
    base
}

pub fn slots(model: usize, window_bits: u8) -> usize {
    let slots = if window_bits == 0 {
        1
    } else {
        2 * window_bits as usize
    };
    SLOT_LIMITS[model].min(slots)
}

pub fn farthest(model: usize, window_bits: u8) -> usize {
    slot_table()[slots(model, window_bits)]
}

struct Model {
    totals: Vec<u16>,
    symbols: Vec<u16>,
    until_sort: u32,
    sorts: usize,
}

impl Model {
    fn new(count: usize) -> Self {
        Model {
            totals: (0..=count).rev().map(|total| total as u16).collect(),
            symbols: (0..count as u16).collect(),
            until_sort: 4,
            sorts: 0,
        }
    }

    fn count(&self) -> usize {
        self.symbols.len()
    }

    fn rescale(&mut self) {
        let count = self.count();
        self.until_sort -= 1;
        if self.until_sort != 0 {
            let mut floor = 1u16;
            for index in (0..count).rev() {
                let halved = (self.totals[index] / 2).max(floor);
                self.totals[index] = halved;
                floor = halved + 1;
            }
            return;
        }
        self.until_sort = 50;
        self.sorts += 1;
        let mut frequencies: Vec<u16> = (0..count)
            .map(|index| (self.totals[index] - self.totals[index + 1]).div_ceil(2))
            .collect();
        for index in 0..count {
            for other in index + 1..count {
                if frequencies[index] < frequencies[other] {
                    frequencies.swap(index, other);
                    self.symbols.swap(index, other);
                }
            }
        }
        let mut sum = 0u16;
        for index in (0..count).rev() {
            sum += frequencies[index];
            self.totals[index] = sum;
        }
        self.totals[count] = 0;
    }

    fn encode(&mut self, symbol: usize, coder: &mut Coder) {
        if self.totals[0] > 3800 {
            self.rescale();
        }
        let index = self
            .symbols
            .iter()
            .position(|&known| usize::from(known) == symbol)
            .unwrap_or_else(|| panic!("symbol {symbol} is not in a model of {}", self.count()));
        coder.encode(
            u32::from(self.totals[index + 1]),
            u32::from(self.totals[index]),
            u32::from(self.totals[0]),
        );
        for total in &mut self.totals[..=index] {
            *total += 8;
        }
    }
}

#[derive(Default)]
struct Coder {
    low: u32,
    high: u32,
    pending: usize,
    bits: Vec<bool>,
    shifts: usize,
    raw: Vec<(usize, u32, u32)>,
}

impl Coder {
    fn new() -> Self {
        Coder {
            high: 0xFFFF,
            ..Coder::default()
        }
    }

    fn emit(&mut self, bit: bool) {
        self.bits.push(bit);
        for _ in 0..self.pending {
            self.bits.push(!bit);
        }
        self.pending = 0;
    }

    fn encode(&mut self, start: u32, end: u32, total: u32) {
        let range = self.high - self.low + 1;
        self.high = self.low + end * range / total - 1;
        self.low += start * range / total;
        loop {
            if self.high < 0x8000 {
                self.emit(false);
            } else if self.low >= 0x8000 {
                self.emit(true);
                self.low -= 0x8000;
                self.high -= 0x8000;
            } else if self.low >= 0x4000 && self.high < 0xC000 {
                self.pending += 1;
                self.low -= 0x4000;
                self.high -= 0x4000;
            } else {
                break;
            }
            self.low *= 2;
            self.high = 2 * self.high + 1;
            self.shifts += 1;
        }
    }

    fn raw(&mut self, value: usize, count: u32) {
        assert!(value < 1 << count);
        if count > 0 {
            self.raw.push((16 + self.shifts, value as u32, count));
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.pending += 1;
        let low_quarter = self.low < 0x4000;
        self.emit(!low_quarter);
        let needed = 16 + self.shifts;
        self.bits.resize(needed.max(self.bits.len()), false);
        let mut stream = Vec::new();
        let mut raw = self.raw.iter().peekable();
        for (index, &bit) in self.bits[..needed].iter().enumerate() {
            while let Some(&&(at, value, count)) = raw.peek() {
                if at != index {
                    break;
                }
                stream.extend((0..count).rev().map(|shift| value >> shift & 1 == 1));
                raw.next();
            }
            stream.push(bit);
        }
        for &(at, value, count) in raw {
            assert_eq!(at, needed);
            stream.extend((0..count).rev().map(|shift| value >> shift & 1 == 1));
        }
        let length = (stream.len() + 2).div_ceil(8);
        stream.resize(8 * length, false);
        stream
            .chunks(8)
            .map(|bits| {
                bits.iter()
                    .fold(0u8, |byte, &bit| byte << 1 | u8::from(bit))
            })
            .collect()
    }
}

struct Models {
    window_bits: u8,
    selector: Model,
    literals: Vec<Model>,
    slots: Vec<Model>,
    lengths: Model,
}

impl Models {
    fn new(window_bits: u8) -> Self {
        Models {
            window_bits,
            selector: Model::new(7),
            literals: (0..4).map(|_| Model::new(64)).collect(),
            slots: (0..3)
                .map(|model| Model::new(slots(model, window_bits)))
                .collect(),
            lengths: Model::new(27),
        }
    }

    fn token(&mut self, token: Token, coder: &mut Coder) {
        let (length, distance) = match token {
            Token::Literal(byte) => {
                let model = usize::from(byte >> 6);
                self.selector.encode(model, coder);
                self.literals[model].encode(usize::from(byte & 63), coder);
                return;
            }
            Token::Match { length, distance } => (length, distance),
        };
        let model = match length {
            3 => 0,
            4 => 1,
            5..=LONGEST_MATCH => 2,
            _ => panic!("Quantum has no match of {length} bytes"),
        };
        self.selector.encode(4 + model, coder);
        if model == 2 {
            let base = length_table();
            let symbol = (0..27).rfind(|&symbol| base[symbol] <= length).unwrap();
            self.lengths.encode(symbol, coder);
            coder.raw(length - base[symbol], LENGTH_EXTRA[symbol]);
        }
        let value = distance - 1;
        let base = slot_table();
        let slot = (0..42).rfind(|&slot| base[slot] <= value).unwrap();
        assert!(
            slot < slots(model, self.window_bits),
            "a match of {length} bytes cannot reach {distance} bytes back"
        );
        self.slots[model].encode(slot, coder);
        coder.raw(value - base[slot], slot_extra(slot));
    }

    fn sorts(&self) -> [usize; MODELS] {
        let mut sorts = [self.selector.sorts, 0, 0, 0, 0, 0, 0, 0, self.lengths.sorts];
        for (index, model) in self.literals.iter().chain(&self.slots).enumerate() {
            sorts[1 + index] = model.sorts;
        }
        sorts
    }
}

pub fn encode_tokens_counting_sorts(
    window_bits: u8,
    chunks: &[Vec<Token>],
) -> (Vec<Chunk>, [usize; MODELS]) {
    let mut models = Models::new(window_bits);
    let encoded = chunks
        .iter()
        .map(|tokens| {
            let mut coder = Coder::new();
            for &token in tokens {
                models.token(token, &mut coder);
            }
            Chunk {
                bytes: coder.finish(),
                size: tokens.iter().map(Token::size).sum(),
            }
        })
        .collect();
    (encoded, models.sorts())
}

pub fn encode_tokens(window_bits: u8, chunks: &[Vec<Token>]) -> Vec<Chunk> {
    encode_tokens_counting_sorts(window_bits, chunks).0
}

struct Random(u64);

impl Random {
    fn odds(&mut self, percent: u64) -> bool {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % 100 < percent
    }

    fn below(&mut self, limit: usize) -> usize {
        self.odds(0);
        (self.0 % limit as u64) as usize
    }
}

fn representable(length: usize, distance: usize, window_bits: u8) -> Option<usize> {
    let reach = |model| distance <= farthest(model, window_bits);
    match length {
        0..=2 => None,
        3 => reach(0).then_some(3),
        4 if reach(1) => Some(4),
        4 => reach(0).then_some(3),
        _ if reach(2) => Some(length),
        _ if reach(1) => Some(4),
        _ => reach(0).then_some(3),
    }
}

pub fn parse(data: &[u8], options: &Options) -> Vec<Vec<Token>> {
    const HASH: usize = 1 << 16;
    let hash = |at: usize| {
        (usize::from(data[at]) << 8 ^ usize::from(data[at + 1]) << 4 ^ usize::from(data[at + 2]))
            % HASH
    };
    let reach_limit = farthest(2, options.window_bits);
    let mut random = Random(options.seed | 1);
    let mut head = vec![usize::MAX; HASH];
    let mut previous = vec![usize::MAX; data.len()];
    let mut chunks = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let chunk_end = (at + CHUNK).min(data.len());
        let mut tokens = Vec::new();
        while at < chunk_end {
            let longest = (chunk_end - at).min(LONGEST_MATCH);
            let mut best = (0, 0);
            if longest >= 3 && at + 2 < data.len() {
                let mut candidate = head[hash(at)];
                let mut steps = 0;
                while candidate != usize::MAX && steps < options.chain {
                    let distance = at - candidate;
                    if distance > reach_limit {
                        break;
                    }
                    let length = (0..longest)
                        .take_while(|&index| data[candidate + index] == data[at + index])
                        .count();
                    if let Some(length) = representable(length, distance, options.window_bits)
                        && length > best.0
                    {
                        best = (length, distance);
                    }
                    candidate = previous[candidate];
                    steps += 1;
                }
            }
            let (mut length, distance) = best;
            if length >= 3 && random.odds(options.shorten_odds) {
                let shorter = 3 + random.below(length - 2);
                length = representable(shorter, distance, options.window_bits).unwrap_or(length);
            }
            let token = if length < 3 || random.odds(options.literal_odds) {
                Token::Literal(data[at])
            } else {
                Token::Match { length, distance }
            };
            let hashed_end = (at + token.size()).min(data.len().saturating_sub(2));
            for (step, link) in previous.iter_mut().enumerate().take(hashed_end).skip(at) {
                let key = hash(step);
                *link = head[key];
                head[key] = step;
            }
            at += token.size();
            tokens.push(token);
        }
        chunks.push(tokens);
    }
    chunks
}

pub fn encode(data: &[u8], options: &Options) -> Vec<Chunk> {
    encode_tokens(options.window_bits, &parse(data, options))
}
