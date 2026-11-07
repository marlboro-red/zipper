use crate::{BadField, Field, field_check};

pub(crate) const CHUNK: usize = 32 * 1024;
pub(crate) const MAX_WINDOW_BITS: u8 = 21;
const SMALLEST_WINDOW_BITS: u8 = 15;
const LITERAL_MODELS: usize = 4;
const LITERAL_SYMBOLS: usize = 64;
const SELECTORS: usize = LITERAL_MODELS + MATCH_MODELS;
const MATCH_MODELS: usize = 3;
const MOST_SYMBOLS: usize = LITERAL_SYMBOLS;
const LENGTH_SYMBOLS: usize = 27;
const MOST_SLOTS: [usize; MATCH_MODELS] = [24, 36, 42];
const SHORTEST_MATCH: [usize; MATCH_MODELS] = [3, 4, 5];
const LENGTHS_WITHOUT_EXTRA: usize = 6;
const LONGEST_LENGTH_SYMBOL: usize = LENGTH_SYMBOLS - 1;
const SLOTS_WITHOUT_EXTRA: usize = 4;
const UPDATE_STEP: u16 = 8;
const RESCALE_ABOVE: u16 = 3800;
const FIRST_REORDER: u8 = 4;
const REORDER_EVERY: u8 = 50;
const CODE_BITS: u32 = 16;
const TOP: u32 = 1 << 15;
const SECOND: u32 = 1 << 14;
const CODE_MASK: u32 = (1 << CODE_BITS) - 1;
// EXTERNAL(7-Zip QuantumDecoder): a block ends with at least 2 zero bits past what the
// decoder read, padded to a whole byte, so at most 9 bits remain.
const LEAST_PADDING: usize = 2;
const MOST_PADDING: usize = 9;
const TABLE_OVERHEAD: u64 = 4 * 1024;

pub(crate) fn window_size(window_bits: u8) -> usize {
    1 << window_bits.max(SMALLEST_WINDOW_BITS)
}

pub(crate) fn decoder_memory(window_bits: u8) -> u64 {
    window_size(window_bits) as u64 + TABLE_OVERHEAD
}

// EXTERNAL(7-Zip 26.05 QuantumDecoder): 7zz reads window levels 0 to 9 too, and gives
// level 0 one distance slot.
fn slot_count(model: usize, window_bits: u8) -> usize {
    MOST_SLOTS[model].min((2 * usize::from(window_bits)).max(1))
}

fn length_extra_bits(symbol: usize) -> u32 {
    if symbol < LENGTHS_WITHOUT_EXTRA || symbol == LONGEST_LENGTH_SYMBOL {
        0
    } else {
        ((symbol - 2) >> 2) as u32
    }
}

fn length_base(symbol: usize) -> usize {
    if symbol < LENGTHS_WITHOUT_EXTRA {
        return symbol + SHORTEST_MATCH[2];
    }
    let step = symbol - 2;
    ((4 | (step & 3)) << (step >> 2)) + 3
}

fn distance_extra_bits(slot: usize) -> u32 {
    if slot < SLOTS_WITHOUT_EXTRA {
        0
    } else {
        ((slot - 2) >> 1) as u32
    }
}

fn distance_base(slot: usize) -> usize {
    if slot < SLOTS_WITHOUT_EXTRA {
        return slot;
    }
    (2 | (slot & 1)) << distance_extra_bits(slot)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Interval {
    low: u32,
    range: u32,
    code: u32,
}

impl Interval {
    fn threshold(&self, total: u32) -> u32 {
        ((self.code + 1) * total - 1) / self.range
    }

    fn narrow(&mut self, start: u32, end: u32, total: u32) -> Field<u32> {
        let below = start * self.range / total;
        let above = end * self.range / total;
        field_check(
            below <= self.code && self.code < above,
            "Quantum code lies outside the interval of its symbol",
        )?;
        Ok(self.normalize(self.low + below, self.low + above - 1, self.code - below))
    }

    fn normalize(&mut self, mut low: u32, mut high: u32, code: u32) -> u32 {
        let mut shifts = 0;
        loop {
            if (low ^ high) & TOP != 0 {
                if low & SECOND == 0 || high & SECOND != 0 {
                    break;
                }
                low &= SECOND - 1;
                high |= SECOND;
            }
            low = (low << 1) & CODE_MASK;
            high = ((high << 1) & CODE_MASK) | 1;
            shifts += 1;
        }
        self.low = low;
        self.range = high - low + 1;
        self.code = code << shifts;
        shifts
    }
}

struct Bits<'a> {
    data: &'a [u8],
    next: usize,
    buffer: u64,
    count: u32,
    consumed: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits {
            data,
            next: 0,
            buffer: 0,
            count: 0,
            consumed: 0,
        }
    }

    fn read(&mut self, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        if self.count < count {
            while self.count <= 56 {
                let byte = self.data.get(self.next).copied().unwrap_or(0);
                self.buffer |= u64::from(byte) << (56 - self.count);
                self.count += 8;
                self.next += 1;
            }
        }
        let value = (self.buffer >> (64 - count)) as u32;
        self.buffer <<= count;
        self.count -= count;
        self.consumed += count as usize;
        value
    }

    fn ensure_in_input(&self) -> Field<()> {
        field_check(
            self.consumed <= 8 * self.data.len(),
            "truncated Quantum block",
        )
    }

    fn finish(&mut self) -> Field<()> {
        let left = (8 * self.data.len()).checked_sub(self.consumed);
        field_check(
            left.is_some_and(|left| left >= LEAST_PADDING),
            "truncated Quantum block",
        )?;
        let left = left.unwrap_or(0);
        field_check(
            left <= MOST_PADDING,
            "Quantum block has bytes after its data",
        )?;
        field_check(
            self.read(left as u32) == 0,
            "Quantum block ends with bits that are not zero",
        )
    }
}

struct Coder<'a> {
    interval: Interval,
    bits: Bits<'a>,
}

impl<'a> Coder<'a> {
    fn new(data: &'a [u8]) -> Self {
        let mut bits = Bits::new(data);
        let code = bits.read(CODE_BITS);
        Coder {
            interval: Interval {
                low: 0,
                range: 1 << CODE_BITS,
                code,
            },
            bits,
        }
    }

    fn narrow(&mut self, start: u32, end: u32, total: u32) -> Field<()> {
        let shifts = self.interval.narrow(start, end, total)?;
        self.interval.code |= self.bits.read(shifts);
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Model {
    count: usize,
    reorders_left: u8,
    totals: [u16; MOST_SYMBOLS + 1],
    symbols: [u8; MOST_SYMBOLS],
}

impl Model {
    fn new(count: usize, first: usize) -> Self {
        let mut model = Model {
            count,
            reorders_left: FIRST_REORDER,
            totals: [0; MOST_SYMBOLS + 1],
            symbols: [0; MOST_SYMBOLS],
        };
        for index in 0..count {
            model.totals[index] = (count - index) as u16;
            model.symbols[index] = (first + index) as u8;
        }
        model
    }

    fn rescale(&mut self) {
        let count = self.count;
        self.reorders_left -= 1;
        if self.reorders_left > 0 {
            for index in (0..count).rev() {
                self.totals[index] = (self.totals[index] >> 1).max(self.totals[index + 1] + 1);
            }
            return;
        }
        self.reorders_left = REORDER_EVERY;
        for index in 0..count {
            self.totals[index] = (self.totals[index] - self.totals[index + 1] + 1) >> 1;
        }
        for index in 0..count.saturating_sub(1) {
            for later in index + 1..count {
                if self.totals[index] < self.totals[later] {
                    self.totals.swap(index, later);
                    self.symbols.swap(index, later);
                }
            }
        }
        for index in (0..count).rev() {
            self.totals[index] += self.totals[index + 1];
        }
    }

    fn pick(&mut self, threshold: u32) -> (usize, u32, u32) {
        let mut index = 1;
        while index < self.count && u32::from(self.totals[index]) > threshold {
            index += 1;
        }
        let start = u32::from(self.totals[index]);
        let end = u32::from(self.totals[index - 1]);
        for total in &mut self.totals[..index] {
            *total += UPDATE_STEP;
        }
        (index - 1, start, end)
    }

    fn decode(&mut self, coder: &mut Coder) -> Field<usize> {
        if self.totals[0] > RESCALE_ABOVE {
            self.rescale();
        }
        let total = u32::from(self.totals[0]);
        let threshold = coder.interval.threshold(total);
        let (index, start, end) = self.pick(threshold);
        coder.narrow(start, end, total)?;
        Ok(usize::from(self.symbols[index]))
    }
}

pub(crate) struct Quantum {
    window: Vec<u8>,
    window_bits: u8,
    position: usize,
    wrapped: bool,
    selector: Model,
    literals: [Model; LITERAL_MODELS],
    slots: [Model; MATCH_MODELS],
    lengths: Model,
}

impl Quantum {
    pub(crate) fn new(window_bits: u8) -> Field<Self> {
        field_check(
            window_bits <= MAX_WINDOW_BITS,
            "Quantum window is larger than 2^21 bytes",
        )?;
        let mut quantum = Quantum {
            window: vec![0; window_size(window_bits)],
            window_bits,
            position: 0,
            wrapped: false,
            selector: Model::new(SELECTORS, 0),
            literals: [Model::new(0, 0); LITERAL_MODELS],
            slots: [Model::new(0, 0); MATCH_MODELS],
            lengths: Model::new(LENGTH_SYMBOLS, 0),
        };
        quantum.reset();
        Ok(quantum)
    }

    pub(crate) fn reset_to_window(&mut self, window_bits: u8) -> Field<()> {
        field_check(
            window_bits <= MAX_WINDOW_BITS,
            "Quantum window is larger than 2^21 bytes",
        )?;
        if self.window.len() != window_size(window_bits) {
            self.window = vec![0; window_size(window_bits)];
        }
        self.window_bits = window_bits;
        self.reset();
        Ok(())
    }

    pub(crate) fn reset(&mut self) {
        self.position = 0;
        self.wrapped = false;
        self.selector = Model::new(SELECTORS, 0);
        for (index, model) in self.literals.iter_mut().enumerate() {
            *model = Model::new(LITERAL_SYMBOLS, index * LITERAL_SYMBOLS);
        }
        for (index, model) in self.slots.iter_mut().enumerate() {
            *model = Model::new(slot_count(index, self.window_bits), 0);
        }
        self.lengths = Model::new(LENGTH_SYMBOLS, 0);
    }

    pub(crate) fn chunk(&mut self, data: &[u8], size: usize) -> Field<&[u8]> {
        field_check(
            (1..=CHUNK).contains(&size),
            "Quantum block is empty or larger than 32 KiB",
        )?;
        field_check(
            data.len() * 8 >= CODE_BITS as usize,
            "truncated Quantum block",
        )?;
        if self.position == self.window.len() {
            self.position = 0;
            self.wrapped = true;
        }
        field_check(
            size <= self.window.len() - self.position,
            "Quantum block runs past the end of its window",
        )?;
        let mut coder = Coder::new(data);
        let start = self.position;
        let end = start + size;
        while self.position < end {
            coder.bits.ensure_in_input()?;
            let selector = self.selector.decode(&mut coder)?;
            if selector < LITERAL_MODELS {
                self.window[self.position] = self.literals[selector].decode(&mut coder)? as u8;
                self.position += 1;
                continue;
            }
            let model = selector - LITERAL_MODELS;
            let length = if model == MATCH_MODELS - 1 {
                let symbol = self.lengths.decode(&mut coder)?;
                length_base(symbol) + coder.bits.read(length_extra_bits(symbol)) as usize
            } else {
                SHORTEST_MATCH[model]
            };
            let slot = self.slots[model].decode(&mut coder)?;
            let distance =
                distance_base(slot) + coder.bits.read(distance_extra_bits(slot)) as usize + 1;
            self.copy(length, distance, end)?;
        }
        coder.bits.finish()?;
        Ok(&self.window[start..end])
    }

    fn copy(&mut self, length: usize, distance: usize, end: usize) -> Field<()> {
        field_check(
            length <= end - self.position,
            "Quantum match runs past the end of its block",
        )?;
        let source = if distance <= self.position {
            self.position - distance
        } else if self.wrapped {
            self.position + self.window.len() - distance
        } else {
            return Err(BadField(
                "Quantum match reaches before the start of its folder",
            ));
        };
        if distance >= length && source + length <= self.window.len() {
            self.window
                .copy_within(source..source + length, self.position);
            self.position += length;
            return Ok(());
        }
        let mut from = source;
        for _ in 0..length {
            self.window[self.position] = self.window[from];
            self.position += 1;
            from += 1;
            if from == self.window.len() {
                from = 0;
            }
        }
        Ok(())
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const LARGEST_PROVEN_MODEL: usize = 5;

    fn falling(model: &Model) -> bool {
        (0..model.count).all(|index| model.totals[index] > model.totals[index + 1])
            && model.totals[model.count] == 0
    }

    #[kani::proof]
    #[kani::unwind(7)]
    fn models_pick_a_symbol_inside_their_totals_and_keep_them_falling() {
        let count: usize = kani::any();
        kani::assume((1..=LARGEST_PROVEN_MODEL).contains(&count));
        let mut model = Model::new(count, 0);
        for index in 0..count {
            model.totals[index] = kani::any();
        }
        model.symbols = kani::any();
        model.reorders_left = kani::any();
        kani::assume((1..=REORDER_EVERY).contains(&model.reorders_left));
        kani::assume(falling(&model) && model.totals[0] <= RESCALE_ABOVE + UPDATE_STEP);
        if model.totals[0] > RESCALE_ABOVE {
            model.rescale();
            assert!(falling(&model));
            assert!((1..=REORDER_EVERY).contains(&model.reorders_left));
        }
        let total = u32::from(model.totals[0]);
        assert!(total >= count as u32 && total <= u32::from(RESCALE_ABOVE));
        let threshold: u32 = kani::any();
        kani::assume(threshold < total);
        let (index, start, end) = model.pick(threshold);
        assert!(index < count);
        assert!(start <= threshold && threshold < end && end <= total);
        assert!(falling(&model) && model.totals[0] <= RESCALE_ABOVE + UPDATE_STEP);
    }

    #[kani::proof]
    #[kani::unwind(18)]
    fn normalizing_keeps_the_code_inside_the_range_for_every_interval() {
        let low: u32 = kani::any();
        let high: u32 = kani::any();
        let code: u32 = kani::any();
        kani::assume(low <= high && high <= CODE_MASK && code <= high - low);
        let mut interval = Interval {
            low: 0,
            range: 1,
            code: 0,
        };
        let shifts = interval.normalize(low, high, code);
        assert!(shifts <= CODE_BITS);
        let bits: u32 = kani::any();
        kani::assume(bits < 1 << shifts);
        interval.code |= bits;
        assert!(interval.range > SECOND && interval.range <= 1 << CODE_BITS);
        assert!(interval.low + interval.range <= 1 << CODE_BITS);
        assert!(interval.code < interval.range);
        assert!(u64::from(interval.code + 1) * u64::from(RESCALE_ABOVE) < 1 << 32);
    }

    #[kani::proof]
    fn lengths_and_distances_stay_inside_the_block_and_window() {
        let symbol: usize = kani::any();
        kani::assume(symbol < LENGTH_SYMBOLS);
        let extra: usize = kani::any();
        kani::assume(extra < 1 << length_extra_bits(symbol));
        let length = length_base(symbol) + extra;
        assert!(length >= SHORTEST_MATCH[2] && length <= 259);
        let window_bits: u8 = kani::any();
        kani::assume(window_bits <= MAX_WINDOW_BITS);
        let model: usize = kani::any();
        kani::assume(model < MATCH_MODELS);
        let slots = slot_count(model, window_bits);
        assert!(slots <= MOST_SYMBOLS);
        let slot: usize = kani::any();
        kani::assume(slot < slots);
        assert!(distance_extra_bits(slot) <= 19);
        let extra: usize = kani::any();
        kani::assume(extra < 1 << distance_extra_bits(slot));
        let distance = distance_base(slot) + extra + 1;
        assert!(distance <= (1 << window_bits).max(1) && distance <= window_size(window_bits));
    }

    #[kani::proof]
    #[kani::unwind(10)]
    fn bits_never_panic_and_count_every_bit_they_read() {
        let data: [u8; 4] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= data.len());
        let mut bits = Bits::new(&data[..length]);
        let first: u32 = kani::any();
        let second: u32 = kani::any();
        kani::assume(first <= 19 && second <= 19);
        assert!(u64::from(bits.read(first)) < 1 << first);
        assert!(u64::from(bits.read(second)) < 1 << second);
        assert!(bits.consumed == (first + second) as usize);
        let _ = bits.ensure_in_input();
        let _ = bits.finish();
    }
}

#[cfg(test)]
#[path = "../tests/common/quantum_encoder.rs"]
mod encoder;

#[cfg(test)]
mod tests {
    use super::*;
    use encoder::Token;

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

    fn decode_all(window_bits: u8, chunks: &[encoder::Chunk]) -> Field<Vec<u8>> {
        let mut quantum = Quantum::new(window_bits)?;
        let mut output = Vec::new();
        for chunk in chunks {
            output.extend_from_slice(quantum.chunk(&chunk.bytes, chunk.size)?);
        }
        Ok(output)
    }

    fn sample(random: &mut Random, length: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(length);
        while data.len() < length {
            match random.below(5) {
                0 => data.extend((0..random.below(64)).map(|_| random.next() as u8)),
                1 if !data.is_empty() => {
                    let back = 1 + random.below(data.len().min(3 << 20));
                    for _ in 0..3 + random.below(300) {
                        data.push(data[data.len() - back]);
                    }
                }
                2 => data.extend(std::iter::repeat_n(
                    random.next() as u8 % 3,
                    random.below(300),
                )),
                3 => data.extend((0..random.below(200)).map(|_| b'a' + random.next() as u8 % 4)),
                _ => data.extend_from_slice(b"Quantum folders hold the files of a cabinet. "),
            }
        }
        data.truncate(length);
        data
    }

    fn literals(tokens: &[Token]) -> Vec<u8> {
        let mut output = Vec::new();
        for token in tokens {
            match *token {
                Token::Literal(byte) => output.push(byte),
                Token::Match { length, distance } => {
                    for _ in 0..length {
                        output.push(output[output.len() - distance]);
                    }
                }
            }
        }
        output
    }

    #[test]
    fn every_encoded_input_decodes_to_itself_at_every_window() {
        let mut random = Random(0x5eed_0000_0000_0001);
        for case in 0..240 {
            let window_bits = (case % 22) as u8;
            let options = encoder::Options::random(&mut || random.next(), window_bits);
            let length = match random.below(4) {
                0 => 1 + random.below(100),
                1 => 1 + random.below(3 * CHUNK),
                2 => CHUNK * (1 + random.below(3)),
                _ => 1 + random.below(window_size(window_bits) * 2 + 3 * CHUNK),
            };
            let data = sample(&mut random, length);
            let chunks = encoder::encode(&data, &options);
            assert_eq!(
                decode_all(window_bits, &chunks)
                    .unwrap_or_else(|error| panic!("case {case}: {error}; {options:?}")),
                data,
                "case {case}: {options:?}"
            );
        }
    }

    #[test]
    fn every_model_reorders_and_still_decodes() {
        let mut random = Random(0x0dd5_0000_0000_0007);
        let window_bits = MAX_WINDOW_BITS;
        let mut chunks = Vec::new();
        let mut written = 0usize;
        for _ in 0..96 {
            let mut tokens = Vec::new();
            let mut size = 0;
            while size < CHUNK {
                let room = CHUNK - size;
                let choice = random.below(4);
                let token = if choice == 0 || room < encoder::LONGEST_MATCH {
                    Token::Literal(random.next() as u8)
                } else {
                    let model = choice - 1;
                    let length = match model {
                        0 => 3,
                        1 => 4,
                        _ => 5 + random.below(encoder::LONGEST_MATCH - 4),
                    };
                    let reach = encoder::farthest(model, window_bits).min(written + size);
                    Token::Match {
                        length,
                        distance: 1 + random.below(reach),
                    }
                };
                if token.size() > 1 && written + size == 0 {
                    continue;
                }
                size += token.size();
                tokens.push(token);
            }
            written += size;
            chunks.push(tokens);
        }
        let (encoded, sorts) = encoder::encode_tokens_counting_sorts(window_bits, &chunks);
        assert!(sorts.iter().all(|&count| count >= 1), "{sorts:?}");
        let expected = literals(&chunks.concat());
        assert_eq!(decode_all(window_bits, &encoded).unwrap(), expected);
    }

    #[test]
    fn matches_before_the_folder_start_or_past_their_block_are_refused() {
        let before = encoder::encode_tokens(
            15,
            &[vec![
                Token::Literal(1),
                Token::Literal(2),
                Token::Match {
                    length: 3,
                    distance: 3,
                },
            ]],
        );
        assert_eq!(
            decode_all(15, &before).err().map(|error| error.0),
            Some("Quantum match reaches before the start of its folder")
        );
        let mut past = encoder::encode_tokens(
            15,
            &[vec![
                Token::Literal(1),
                Token::Match {
                    length: 3,
                    distance: 1,
                },
            ]],
        );
        assert_eq!(decode_all(15, &past).unwrap(), [1, 1, 1, 1]);
        past[0].size = 3;
        assert_eq!(
            decode_all(15, &past).err().map(|error| error.0),
            Some("Quantum match runs past the end of its block")
        );
    }

    #[test]
    fn a_match_after_the_window_wraps_reads_the_previous_lap() {
        let window_bits = 15;
        let mut random = Random(77);
        let first: Vec<Token> = (0..CHUNK)
            .map(|_| Token::Literal(random.next() as u8))
            .collect();
        let second = vec![
            Token::Match {
                length: 259,
                distance: CHUNK,
            },
            Token::Match {
                length: 4,
                distance: 100,
            },
        ];
        let expected = literals(&[first.clone(), second.clone()].concat());
        let chunks = encoder::encode_tokens(window_bits, &[first, second]);
        assert_eq!(decode_all(window_bits, &chunks).unwrap(), expected);
    }

    #[test]
    fn padding_must_be_zero_and_end_the_block() {
        let chunks = encoder::encode_tokens(16, &[vec![Token::Literal(7); 50]]);
        assert_eq!(decode_all(16, &chunks).unwrap(), [7; 50]);
        let mut longer = chunks.clone();
        longer[0].bytes.push(0);
        assert_eq!(
            decode_all(16, &longer).err().map(|error| error.0),
            Some("Quantum block has bytes after its data")
        );
        let mut flagged = chunks.clone();
        *flagged[0].bytes.last_mut().unwrap() |= 1;
        assert_eq!(
            decode_all(16, &flagged).err().map(|error| error.0),
            Some("Quantum block ends with bits that are not zero")
        );
        let mut cut = chunks;
        cut[0].bytes.pop();
        assert_eq!(
            decode_all(16, &cut).err().map(|error| error.0),
            Some("truncated Quantum block")
        );
    }

    #[test]
    fn windows_above_2_21_are_refused() {
        for bits in [22, 23, 255] {
            assert!(Quantum::new(bits).is_err(), "{bits}");
        }
        for bits in 0..=MAX_WINDOW_BITS {
            let quantum = Quantum::new(bits).unwrap();
            assert_eq!(quantum.window.len(), 1 << bits.max(15));
            assert!(decoder_memory(bits) <= (1 << 21) + TABLE_OVERHEAD);
        }
        assert!(std::mem::size_of::<Quantum>() as u64 <= TABLE_OVERHEAD);
    }

    #[test]
    fn a_threshold_always_picks_an_interval_that_holds_the_code() {
        let mut random = Random(0x7e57);
        let mut ranges: Vec<u32> = (SECOND + 1..=1 << CODE_BITS).step_by(61).collect();
        ranges.extend([
            SECOND + 1,
            SECOND + 2,
            1 << 15,
            (1 << CODE_BITS) - 1,
            1 << CODE_BITS,
        ]);
        for range in ranges {
            for total in 1..=u32::from(RESCALE_ABOVE) {
                let random_code = (random.next() % u64::from(range)) as u32;
                for code in [0, 1, range / 2, range - 2, range - 1, random_code] {
                    let interval = Interval {
                        low: 0,
                        range,
                        code,
                    };
                    let threshold = interval.threshold(total);
                    assert!(threshold < total);
                    assert!(threshold * range / total <= code, "{range} {total} {code}");
                    assert!(
                        code < (threshold + 1) * range / total,
                        "{range} {total} {code}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_short_chunk_decodes_or_is_refused_without_panicking() {
        for size in [1, 2, 7, CHUNK] {
            for first in 0..=255u8 {
                for second in 0..=255u8 {
                    let mut quantum = Quantum::new(10).unwrap();
                    let _ = quantum.chunk(&[first, second], size);
                    let _ = quantum.chunk(&[first], size);
                }
            }
        }
        let mut random = Random(0xdead_beef);
        for _ in 0..20_000 {
            let bits = random.below(22) as u8;
            let mut quantum = Quantum::new(bits).unwrap();
            for _ in 0..1 + random.below(3) {
                let data: Vec<u8> = (0..random.below(40)).map(|_| random.next() as u8).collect();
                let _ = quantum.chunk(&data, 1 + random.below(CHUNK));
            }
        }
    }

    #[test]
    fn every_cut_is_refused_and_no_damaged_byte_panics() {
        let mut random = Random(0xc0de);
        let data = sample(&mut random, 3 * CHUNK + 999);
        let chunks = encoder::encode(&data, &encoder::Options::plain(16));
        assert_eq!(decode_all(16, &chunks).unwrap(), data);
        for index in 0..chunks.len() {
            for cut in 0..chunks[index].bytes.len() {
                let mut damaged = chunks.clone();
                damaged[index].bytes.truncate(cut);
                assert!(
                    decode_all(16, &damaged).is_err(),
                    "chunk {index} cut at {cut}"
                );
            }
            for _ in 0..400 {
                let mut damaged = chunks.clone();
                let at = random.below(damaged[index].bytes.len());
                damaged[index].bytes[at] ^= 1 << random.below(8);
                if let Ok(output) = decode_all(16, &damaged) {
                    assert_eq!(output.len(), data.len());
                }
            }
        }
    }
}
