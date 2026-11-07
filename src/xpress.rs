use crate::{Field, field_check};

pub(crate) const LONGEST_CODE: u32 = 15;
const TABLE_BITS: u32 = 10;
const LENGTH_SHIFT: u32 = 12;
const SYMBOL_MASK: u16 = (1 << LENGTH_SHIFT) - 1;
pub(crate) const MOST_SYMBOLS: usize = 1 << LENGTH_SHIFT;
const LITERALS: usize = 256;
const SYMBOLS: usize = 512;
const LENGTH_TABLE: usize = SYMBOLS / 2;
const FIRST_BITS: usize = 4;
const END_OF_DATA: usize = LITERALS;
const SHORT_LENGTH_LIMIT: usize = 15;
const BYTE_LENGTH_LIMIT: usize = 0xFF;
const MIN_MATCH: usize = 3;
pub(crate) const MIN_CHUNK_BITS: u32 = 12;
pub(crate) const MAX_CHUNK_BITS: u32 = 16;

pub(crate) struct Huffman {
    table_bits: u32,
    table: Vec<u16>,
    counts: [u32; LONGEST_CODE as usize + 1],
    first: [u32; LONGEST_CODE as usize + 1],
    start: [u32; LONGEST_CODE as usize + 1],
    sorted: Vec<u16>,
}

impl Huffman {
    pub(crate) fn new() -> Self {
        Self::with_table_bits(TABLE_BITS)
    }

    fn with_table_bits(table_bits: u32) -> Self {
        Huffman {
            table_bits,
            table: vec![0; 1 << table_bits],
            counts: [0; LONGEST_CODE as usize + 1],
            first: [0; LONGEST_CODE as usize + 1],
            start: [0; LONGEST_CODE as usize + 1],
            sorted: Vec::new(),
        }
    }

    pub(crate) fn build(&mut self, lengths: &[u8]) -> Field<()> {
        field_check(
            lengths.len() <= MOST_SYMBOLS,
            "Huffman table has too many symbols",
        )?;
        self.counts = [0; LONGEST_CODE as usize + 1];
        for &length in lengths {
            field_check(
                u32::from(length) <= LONGEST_CODE,
                "Huffman code is longer than 15 bits",
            )?;
            self.counts[usize::from(length)] += 1;
        }
        self.counts[0] = 0;
        let space: u32 = (1..=LONGEST_CODE as usize)
            .map(|length| self.counts[length] << (LONGEST_CODE as usize - length))
            .sum();
        field_check(
            space == 1 << LONGEST_CODE,
            "Huffman table is not a complete code",
        )?;
        let mut code = 0u32;
        let mut index = 0u32;
        for length in 1..=LONGEST_CODE as usize {
            self.first[length] = code;
            self.start[length] = index;
            code = (code + self.counts[length]) << 1;
            index += self.counts[length];
        }
        self.sorted.clear();
        self.sorted.resize(index as usize, 0);
        self.table.fill(0);
        let mut next = self.start;
        let mut codes = self.first;
        for (symbol, &length) in lengths.iter().enumerate() {
            let length = u32::from(length);
            if length == 0 {
                continue;
            }
            let slot = length as usize;
            self.sorted[next[slot] as usize] = symbol as u16;
            next[slot] += 1;
            let code = codes[slot];
            codes[slot] += 1;
            if length <= self.table_bits {
                let shift = self.table_bits - length;
                let from = (code << shift) as usize;
                let entry = symbol as u16 | (length as u16) << LENGTH_SHIFT;
                self.table[from..from + (1 << shift)].fill(entry);
            }
        }
        Ok(())
    }

    pub(crate) fn decode(&self, next_bits: u32) -> (usize, u32) {
        let entry = self.table[(next_bits >> (LONGEST_CODE - self.table_bits)) as usize];
        let length = u32::from(entry >> LENGTH_SHIFT);
        if length != 0 {
            return (usize::from(entry & SYMBOL_MASK), length);
        }
        for length in self.table_bits + 1..=LONGEST_CODE {
            let slot = length as usize;
            let index = (next_bits >> (LONGEST_CODE - length)).wrapping_sub(self.first[slot]);
            if index < self.counts[slot] {
                return (
                    usize::from(self.sorted[(self.start[slot] + index) as usize]),
                    length,
                );
            }
        }
        unreachable!("a complete code decodes every 15-bit value")
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u32 {
    u32::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
}

struct Bits<'a> {
    input: &'a [u8],
    next: usize,
    value: u32,
    used: u32,
}

impl<'a> Bits<'a> {
    fn consume(&mut self, count: u32) -> Field<()> {
        self.value <<= count;
        self.used += count;
        if self.used > 16 {
            field_check(self.next + 2 <= self.input.len(), "truncated XPRESS chunk")?;
            self.used -= 16;
            self.value |= u16_at(self.input, self.next) << self.used;
            self.next += 2;
        }
        Ok(())
    }

    fn byte(&mut self) -> Field<usize> {
        field_check(self.next < self.input.len(), "truncated XPRESS chunk")?;
        self.next += 1;
        Ok(usize::from(self.input[self.next - 1]))
    }

    fn word(&mut self) -> Field<usize> {
        field_check(self.next + 2 <= self.input.len(), "truncated XPRESS chunk")?;
        self.next += 2;
        Ok(u16_at(self.input, self.next - 2) as usize)
    }
}

fn match_length(bits: &mut Bits, symbol: usize) -> Field<usize> {
    let short = symbol & 0xF;
    if short < SHORT_LENGTH_LIMIT {
        return Ok(short + MIN_MATCH);
    }
    let byte = bits.byte()?;
    let length = if byte == BYTE_LENGTH_LIMIT {
        bits.word()?
    } else {
        byte + SHORT_LENGTH_LIMIT
    };
    Ok(length + MIN_MATCH)
}

// EXTERNAL(7-Zip XpressDecoder): a chunk is 256 bytes of code lengths and a stream of
// 16-bit words; after its last byte it must decode symbol 256 with only zero bits left.
pub(crate) fn decode(huffman: &mut Huffman, input: &[u8], output: &mut [u8]) -> Field<()> {
    field_check(
        input.len() >= LENGTH_TABLE + 4,
        "XPRESS chunk is shorter than its code lengths",
    )?;
    let mut lengths = [0u8; SYMBOLS];
    for (pair, &byte) in lengths.chunks_exact_mut(2).zip(&input[..LENGTH_TABLE]) {
        pair[0] = byte & 0xF;
        pair[1] = byte >> FIRST_BITS;
    }
    huffman.build(&lengths)?;
    let mut bits = Bits {
        input,
        next: LENGTH_TABLE + 4,
        value: u16_at(input, LENGTH_TABLE) << 16 | u16_at(input, LENGTH_TABLE + 2),
        used: 0,
    };
    let mut at = 0;
    loop {
        let (symbol, length) = huffman.decode(bits.value >> (32 - LONGEST_CODE));
        bits.consume(length)?;
        if at == output.len() {
            return field_check(
                symbol == END_OF_DATA && bits.value == 0 && bits.next == input.len(),
                "XPRESS chunk does not end with its end symbol and its last byte",
            );
        }
        if symbol < LITERALS {
            output[at] = symbol as u8;
            at += 1;
            continue;
        }
        let symbol = symbol - LITERALS;
        let distance_bits = (symbol >> FIRST_BITS) as u32;
        let length = match_length(&mut bits, symbol)?;
        field_check(
            length <= output.len() - at,
            "XPRESS match runs past the end of its chunk",
        )?;
        let distance = if distance_bits == 0 {
            1
        } else {
            let low = bits.value >> (32 - distance_bits);
            bits.consume(distance_bits)?;
            (1usize << distance_bits) + low as usize
        };
        field_check(
            distance <= at,
            "XPRESS match reaches before the start of its chunk",
        )?;
        copy_match(output, at, distance, length);
        at += length;
    }
}

pub(crate) fn copy_match(output: &mut [u8], at: usize, distance: usize, length: usize) {
    if distance >= length {
        output.copy_within(at - distance..at - distance + length, at);
    } else {
        for offset in at..at + length {
            output[offset] = output[offset - distance];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Writer {
        words: Vec<u16>,
        extra: Vec<(usize, Vec<u8>)>,
        value: u32,
        count: u32,
    }

    impl Writer {
        fn bits(&mut self, value: u32, count: u32) {
            for bit in (0..count).rev() {
                self.value = self.value << 1 | (value >> bit) & 1;
                self.count += 1;
                if self.count == 16 {
                    self.words.push(self.value as u16);
                    self.value = 0;
                    self.count = 0;
                }
            }
        }

        fn bytes(&mut self, bytes: &[u8]) {
            let last_loaded = self.words.len() + usize::from(self.count > 0);
            self.extra.push((last_loaded.max(1), bytes.to_vec()));
        }
    }

    enum Token {
        Literal(u8),
        Match { length: usize, distance: usize },
    }

    // EXTERNAL(7-Zip XpressDecoder): an extra length byte follows the last 16-bit word the
    // decoder has loaded, and it keeps one word loaded past the bits it has consumed.
    fn encode(tokens: &[Token]) -> Vec<u8> {
        let mut writer = Writer {
            words: Vec::new(),
            extra: Vec::new(),
            value: 0,
            count: 0,
        };
        for token in tokens {
            match *token {
                Token::Literal(byte) => writer.bits(u32::from(byte), 9),
                Token::Match { length, distance } => {
                    let distance_bits = usize::BITS - 1 - distance.leading_zeros();
                    let short = (length - MIN_MATCH).min(SHORT_LENGTH_LIMIT);
                    let symbol = LITERALS + ((distance_bits as usize) << FIRST_BITS) + short;
                    writer.bits(symbol as u32, 9);
                    if short == SHORT_LENGTH_LIMIT {
                        let rest = length - MIN_MATCH;
                        if rest - SHORT_LENGTH_LIMIT < BYTE_LENGTH_LIMIT {
                            writer.bytes(&[(rest - SHORT_LENGTH_LIMIT) as u8]);
                        } else {
                            let mut bytes = vec![BYTE_LENGTH_LIMIT as u8];
                            bytes.extend((rest as u16).to_le_bytes());
                            writer.bytes(&bytes);
                        }
                    }
                    writer.bits((distance - (1 << distance_bits)) as u32, distance_bits);
                }
            }
        }
        writer.bits(END_OF_DATA as u32, 9);
        while writer.count != 0 {
            writer.bits(0, 1);
        }
        let loaded = (writer.words.len() + 1).max(2);
        writer.words.resize(loaded, 0);
        let mut out = vec![0x99u8; LENGTH_TABLE];
        let mut extra = writer.extra.into_iter().peekable();
        for (index, word) in writer.words.iter().enumerate() {
            out.extend(word.to_le_bytes());
            while let Some((_, bytes)) = extra.next_if(|(at, _)| *at == index) {
                out.extend(bytes);
            }
        }
        assert!(extra.next().is_none());
        out
    }

    fn reference(tokens: &[Token]) -> Vec<u8> {
        let mut out = Vec::new();
        for token in tokens {
            match *token {
                Token::Literal(byte) => out.push(byte),
                Token::Match { length, distance } => {
                    for _ in 0..length {
                        out.push(out[out.len() - distance]);
                    }
                }
            }
        }
        out
    }

    struct Random(u64);

    impl Random {
        fn below(&mut self, limit: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % limit as u64) as usize
        }
    }

    #[test]
    fn flat_codes_with_every_match_shape_decode_to_their_tokens() {
        let mut random = Random(0x2545_f491_4f6c_dd1d);
        let mut huffman = Huffman::new();
        for round in 0..400 {
            let mut tokens = vec![Token::Literal(random.below(256) as u8)];
            let mut size = 1;
            while size < 2000 + round * 10 {
                let token = if random.below(3) == 0 {
                    Token::Literal(random.below(256) as u8)
                } else {
                    let length = match random.below(4) {
                        0 => MIN_MATCH + random.below(15),
                        1 => MIN_MATCH + 15 + random.below(300),
                        2 => MIN_MATCH + 15 + 0xFF + random.below(2000),
                        _ => MIN_MATCH + random.below(40),
                    };
                    let distance = 1 + random.below(size.min(65535));
                    Token::Match { length, distance }
                };
                size += match token {
                    Token::Literal(_) => 1,
                    Token::Match { length, .. } => length,
                };
                tokens.push(token);
            }
            let input = encode(&tokens);
            let expected = reference(&tokens);
            let mut output = vec![0; expected.len()];
            decode(&mut huffman, &input, &mut output).unwrap();
            assert_eq!(output, expected, "round {round}");
            for cut in [1, 2, 3] {
                if input.len() - cut > LENGTH_TABLE + 4 {
                    let mut output = vec![0; expected.len()];
                    assert!(
                        decode(&mut huffman, &input[..input.len() - cut], &mut output).is_err()
                    );
                }
            }
            let mut longer = vec![0; expected.len() + 1];
            assert!(decode(&mut huffman, &input, &mut longer).is_err());
            let mut shorter = vec![0; expected.len() - 1];
            assert!(decode(&mut huffman, &input, &mut shorter).is_err());
        }
    }

    #[test]
    fn every_damaged_byte_is_refused_or_decodes_without_panicking() {
        let mut random = Random(77);
        let tokens: Vec<Token> = (0..300)
            .map(|index| {
                if index % 3 == 0 || index < 10 {
                    Token::Literal(random.below(256) as u8)
                } else {
                    Token::Match {
                        length: MIN_MATCH + random.below(400),
                        distance: 1 + random.below(10),
                    }
                }
            })
            .collect();
        let input = encode(&tokens);
        let size = reference(&tokens).len();
        let mut huffman = Huffman::new();
        for at in 0..input.len() {
            for flip in [1u8, 0x10, 0x80, 0xFF] {
                let mut damaged = input.clone();
                damaged[at] ^= flip;
                let mut output = vec![0; size];
                let _ = decode(&mut huffman, &damaged, &mut output);
            }
        }
    }

    #[test]
    fn a_match_before_the_chunk_or_past_its_end_is_refused() {
        let mut huffman = Huffman::new();
        let early = encode(&[
            Token::Literal(1),
            Token::Match {
                length: 3,
                distance: 2,
            },
        ]);
        let mut output = vec![0; 4];
        assert!(decode(&mut huffman, &early, &mut output).is_err());
        let first = encode(&[Token::Match {
            length: 3,
            distance: 1,
        }]);
        let mut output = vec![0; 3];
        assert!(decode(&mut huffman, &first, &mut output).is_err());
        let long = encode(&[
            Token::Literal(1),
            Token::Match {
                length: 30,
                distance: 1,
            },
        ]);
        let mut output = vec![0; 20];
        assert!(decode(&mut huffman, &long, &mut output).is_err());
    }

    #[test]
    fn incomplete_and_oversubscribed_codes_are_refused() {
        let mut huffman = Huffman::new();
        let mut lengths = [9u8; SYMBOLS];
        assert!(huffman.build(&lengths).is_ok());
        lengths[0] = 10;
        assert!(huffman.build(&lengths).is_err());
        lengths[0] = 8;
        assert!(huffman.build(&lengths).is_err());
        assert!(huffman.build(&[0u8; SYMBOLS]).is_err());
        let mut one = [0u8; SYMBOLS];
        one[7] = 1;
        assert!(huffman.build(&one).is_err());
        one[300] = 1;
        assert!(huffman.build(&one).is_ok());
        assert_eq!(huffman.decode(0), (7, 1));
        assert_eq!(huffman.decode(1 << 14), (300, 1));
    }

    fn lengths_from(random: &mut Random, symbols: usize) -> Vec<u8> {
        loop {
            let mut lengths: Vec<u8> = (0..symbols)
                .map(|_| {
                    if random.below(4) == 0 {
                        0
                    } else {
                        1 + random.below(15) as u8
                    }
                })
                .collect();
            let mut space: i64 = 1 << LONGEST_CODE;
            for length in lengths.iter_mut().filter(|length| **length != 0) {
                let need = 1i64 << (LONGEST_CODE - u32::from(*length));
                if need > space {
                    *length = 0;
                } else {
                    space -= need;
                }
            }
            for length in (1..=LONGEST_CODE as u8).rev() {
                for slot in lengths.iter_mut() {
                    let need = 1i64 << (LONGEST_CODE - u32::from(length));
                    if *slot == 0 && need <= space && space % need == 0 && space > 0 {
                        *slot = length;
                        space -= need;
                    }
                }
            }
            if space == 0 {
                return lengths;
            }
        }
    }

    #[test]
    fn every_code_value_decodes_to_the_symbol_whose_canonical_code_it_starts_with() {
        let mut random = Random(9);
        let mut huffman = Huffman::new();
        for symbols in [2, 3, 8, 54, 256, 512, 799] {
            for _ in 0..8 {
                let lengths = lengths_from(&mut random, symbols);
                huffman.build(&lengths).unwrap();
                let mut order: Vec<(u8, usize)> = lengths
                    .iter()
                    .enumerate()
                    .filter(|(_, length)| **length != 0)
                    .map(|(symbol, &length)| (length, symbol))
                    .collect();
                order.sort();
                let mut code = 0u32;
                let mut previous = 0u8;
                for (length, symbol) in order {
                    code <<= length - previous;
                    previous = length;
                    let shift = LONGEST_CODE - u32::from(length);
                    for low in [0, (1 << shift) - 1] {
                        assert_eq!(
                            huffman.decode(code << shift | low),
                            (symbol, u32::from(length))
                        );
                    }
                    code += 1;
                }
            }
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(17)]
    fn tables_from_any_short_lengths_decode_every_value_to_a_symbol_of_that_code() {
        let lengths: [u8; 6] = kani::any();
        let mut huffman = Huffman::with_table_bits(2);
        if huffman.build(&lengths).is_ok() {
            let value: u32 = kani::any();
            kani::assume(value < 1 << LONGEST_CODE);
            let (symbol, length) = huffman.decode(value);
            assert!(symbol < lengths.len());
            assert!(length >= 1 && length == u32::from(lengths[symbol]));
        }
    }
}
