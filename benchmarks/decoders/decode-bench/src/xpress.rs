use anyhow::{Context, Result, ensure};
use std::ops::Range;
use zipper::bench::Chunk;

pub const CHUNK: usize = 32 * 1024;
const MAGIC: &[u8; 8] = b"ZXPRESS1";
const SYMBOLS: usize = 512;
const LITERALS: usize = 256;
const END_OF_DATA: usize = LITERALS;
const LENGTH_TABLE: usize = SYMBOLS / 2;
const LONGEST_CODE: u32 = 15;
const MIN_MATCH: usize = 3;
const SHORT_LENGTH_LIMIT: usize = 15;
const BYTE_LENGTH_LIMIT: usize = 0xFF;
const HASH_BITS: u32 = 15;
const CHAIN_LIMIT: usize = 32;
const NO_POSITION: usize = usize::MAX;
const LOOKAHEAD_WORDS: usize = 1;
const FIRST_LOADED_WORDS: usize = 2;

#[derive(Clone, Copy)]
enum Token {
    Literal(u8),
    Match { length: usize, distance: usize },
}

impl Token {
    fn symbol(self) -> usize {
        match self {
            Token::Literal(byte) => usize::from(byte),
            Token::Match { length, distance } => {
                LITERALS
                    + ((distance_bits(distance) as usize) << 4)
                    + (length - MIN_MATCH).min(SHORT_LENGTH_LIMIT)
            }
        }
    }
}

fn distance_bits(distance: usize) -> u32 {
    usize::BITS - 1 - distance.leading_zeros()
}

fn hash(bytes: &[u8]) -> usize {
    let value = u32::from(bytes[0]) | u32::from(bytes[1]) << 8 | u32::from(bytes[2]) << 16;
    (value.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

fn tokens(data: &[u8]) -> Vec<Token> {
    let mut head = vec![NO_POSITION; 1 << HASH_BITS];
    let mut previous = vec![NO_POSITION; data.len()];
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let (mut best_length, mut best_distance) = (0, 0);
        if at + MIN_MATCH <= data.len() {
            let mut candidate = head[hash(&data[at..])];
            let mut steps = 0;
            while candidate != NO_POSITION && steps < CHAIN_LIMIT {
                let length = data[candidate..]
                    .iter()
                    .zip(&data[at..])
                    .take_while(|(a, b)| a == b)
                    .count();
                if length > best_length {
                    (best_length, best_distance) = (length, at - candidate);
                }
                candidate = previous[candidate];
                steps += 1;
            }
        }
        let step = if best_length >= MIN_MATCH {
            out.push(Token::Match {
                length: best_length,
                distance: best_distance,
            });
            best_length
        } else {
            out.push(Token::Literal(data[at]));
            1
        };
        for position in (at..at + step).filter(|position| position + MIN_MATCH <= data.len()) {
            let slot = hash(&data[position..]);
            previous[position] = head[slot];
            head[slot] = position;
        }
        at += step;
    }
    out
}

enum Node {
    Leaf(usize),
    Pair(usize, usize),
}

fn code_lengths(frequencies: &[u32; SYMBOLS]) -> [u8; SYMBOLS] {
    let mut leaves: Vec<(u64, usize)> = frequencies
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        .map(|(symbol, &count)| (u64::from(count), symbol))
        .collect();
    if leaves.len() == 1 {
        leaves.push((1, usize::from(leaves[0].1 == 0)));
    }
    leaves.sort_unstable();
    let mut nodes: Vec<Node> = leaves
        .iter()
        .map(|&(_, symbol)| Node::Leaf(symbol))
        .collect();
    let singles: Vec<(u64, usize)> = leaves
        .iter()
        .enumerate()
        .map(|(node, &(weight, _))| (weight, node))
        .collect();
    let mut list = singles.clone();
    for _ in 1..LONGEST_CODE {
        let mut packages = Vec::with_capacity(list.len() / 2);
        for pair in list.as_chunks::<2>().0 {
            nodes.push(Node::Pair(pair[0].1, pair[1].1));
            packages.push((pair[0].0 + pair[1].0, nodes.len() - 1));
        }
        list = merged(&singles, &packages);
    }
    let mut lengths = [0u8; SYMBOLS];
    let mut stack = Vec::new();
    for &(_, node) in package_merge_selection(&list, leaves.len()) {
        stack.push(node);
        while let Some(node) = stack.pop() {
            match nodes[node] {
                Node::Leaf(symbol) => lengths[symbol] += 1,
                Node::Pair(left, right) => stack.extend([left, right]),
            }
        }
    }
    lengths
}

fn package_merge_selection(list: &[(u64, usize)], symbols: usize) -> &[(u64, usize)] {
    &list[..2 * symbols - 2]
}

fn merged(singles: &[(u64, usize)], packages: &[(u64, usize)]) -> Vec<(u64, usize)> {
    let mut out = Vec::with_capacity(singles.len() + packages.len());
    let (mut left, mut right) = (0, 0);
    while left < singles.len() || right < packages.len() {
        let take_single = right == packages.len()
            || (left < singles.len() && singles[left].0 <= packages[right].0);
        if take_single {
            out.push(singles[left]);
            left += 1;
        } else {
            out.push(packages[right]);
            right += 1;
        }
    }
    out
}

fn canonical_codes(lengths: &[u8; SYMBOLS]) -> [u32; SYMBOLS] {
    let mut counts = [0u32; LONGEST_CODE as usize + 1];
    for &length in lengths {
        counts[usize::from(length)] += 1;
    }
    counts[0] = 0;
    let mut next = [0u32; LONGEST_CODE as usize + 1];
    let mut code = 0;
    for (length, first) in next.iter_mut().enumerate().skip(1) {
        code = (code + counts[length - 1]) << 1;
        *first = code;
    }
    let mut codes = [0u32; SYMBOLS];
    for (symbol, &length) in lengths.iter().enumerate() {
        if length != 0 {
            codes[symbol] = next[usize::from(length)];
            next[usize::from(length)] += 1;
        }
    }
    codes
}

struct Bits {
    words: Vec<u16>,
    extra: Vec<(usize, Vec<u8>)>,
    pending: u32,
    count: u32,
}

impl Bits {
    fn put(&mut self, value: u32, count: u32) {
        self.pending = self.pending << count | value;
        self.count += count;
        if self.count >= 16 {
            self.count -= 16;
            self.words.push((self.pending >> self.count) as u16);
            self.pending &= (1 << self.count) - 1;
        }
    }

    // SPEC(MS-XCA 2.2.4): a length byte or word is read from the input after the last
    // 16-bit word the decoder has loaded, and it keeps 32 bits loaded past those consumed.
    fn bytes(&mut self, bytes: &[u8]) {
        let last_loaded = (self.words.len() + usize::from(self.count > 0)).max(1);
        self.extra.push((last_loaded, bytes.to_vec()));
    }
}

// SPEC(MS-XCA 2.2.4): the decoder keeps 32 bits loaded, so one more word follows the
// word that holds the end symbol, and a chunk has at least the two words it loads first.
fn pad_for_lookahead(words: &mut Vec<u16>) {
    let loaded = (words.len() + LOOKAHEAD_WORDS).max(FIRST_LOADED_WORDS);
    words.resize(loaded, 0);
}

pub fn compress_chunk(data: &[u8]) -> Vec<u8> {
    let tokens = tokens(data);
    let mut frequencies = [0u32; SYMBOLS];
    for token in &tokens {
        frequencies[token.symbol()] += 1;
    }
    frequencies[END_OF_DATA] += 1;
    let lengths = code_lengths(&frequencies);
    let codes = canonical_codes(&lengths);
    let mut bits = Bits {
        words: Vec::new(),
        extra: Vec::new(),
        pending: 0,
        count: 0,
    };
    let put_symbol =
        |bits: &mut Bits, symbol: usize| bits.put(codes[symbol], u32::from(lengths[symbol]));
    for &token in &tokens {
        put_symbol(&mut bits, token.symbol());
        if let Token::Match { length, distance } = token {
            let rest = length - MIN_MATCH;
            if rest >= SHORT_LENGTH_LIMIT {
                if rest - SHORT_LENGTH_LIMIT < BYTE_LENGTH_LIMIT {
                    bits.bytes(&[(rest - SHORT_LENGTH_LIMIT) as u8]);
                } else {
                    let mut bytes = vec![BYTE_LENGTH_LIMIT as u8];
                    bytes.extend((rest as u16).to_le_bytes());
                    bits.bytes(&bytes);
                }
            }
            let low_bits = distance_bits(distance);
            bits.put((distance - (1 << low_bits)) as u32, low_bits);
        }
    }
    put_symbol(&mut bits, END_OF_DATA);
    if bits.count > 0 {
        bits.put(0, 16 - bits.count);
    }
    pad_for_lookahead(&mut bits.words);
    let mut out = Vec::with_capacity(LENGTH_TABLE + 2 * bits.words.len());
    out.extend(
        lengths
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| pair[0] | pair[1] << 4),
    );
    let mut extra = bits.extra.into_iter().peekable();
    for (index, word) in bits.words.iter().enumerate() {
        out.extend(word.to_le_bytes());
        while let Some((_, bytes)) = extra.next_if(|(at, _)| *at == index) {
            out.extend(bytes);
        }
    }
    assert!(extra.next().is_none(), "a length byte follows no word");
    out
}

pub fn compress(data: &[u8]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    for chunk in data.chunks(CHUNK) {
        let coded = compress_chunk(chunk);
        let packed = if coded.len() < chunk.len() {
            &coded[..]
        } else {
            chunk
        };
        let header = ChunkHeader {
            decoded: chunk.len() as u32,
            packed: packed.len() as u32,
        };
        out.extend(header.decoded.to_le_bytes());
        out.extend(header.packed.to_le_bytes());
        out.extend(packed);
    }
    out
}

struct ChunkHeader {
    decoded: u32,
    packed: u32,
}

pub struct PackedChunk {
    pub packed: Range<usize>,
    pub decoded_size: usize,
}

impl PackedChunk {
    pub fn is_stored(&self) -> bool {
        self.packed.len() == self.decoded_size
    }
}

pub fn zipper_chunks<'a>(container: &'a [u8], chunks: &[PackedChunk]) -> Vec<Chunk<'a>> {
    chunks
        .iter()
        .map(|chunk| {
            let packed = &container[chunk.packed.clone()];
            if chunk.is_stored() {
                Chunk::Stored(packed)
            } else {
                Chunk::Coded {
                    packed,
                    size: chunk.decoded_size,
                }
            }
        })
        .collect()
}

pub fn chunks(container: &[u8]) -> Result<Vec<PackedChunk>> {
    ensure!(
        container.starts_with(MAGIC),
        "not an XPRESS chunk container"
    );
    let mut at = MAGIC.len();
    let mut out = Vec::new();
    while at < container.len() {
        let field = |offset: usize| -> Result<usize> {
            let bytes = container
                .get(at + offset..at + offset + 4)
                .context("truncated XPRESS chunk container")?;
            Ok(u32::from_le_bytes(bytes.try_into()?) as usize)
        };
        let (size, packed) = (field(0)?, field(4)?);
        let start = at + 8;
        ensure!(
            start + packed <= container.len(),
            "truncated XPRESS chunk container"
        );
        out.push(PackedChunk {
            packed: start..start + packed,
            decoded_size: size,
        });
        at = start + packed;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(seed: u64, size: usize) -> Vec<u8> {
        let mut state = seed;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let words: [&[u8]; 6] = [
            b"zipper ",
            b"decodes ",
            b"every ",
            b"chunk ",
            b"\n",
            b"0123",
        ];
        let mut out = Vec::with_capacity(size);
        while out.len() < size {
            let roll = next();
            match roll % 5 {
                0 => out.push(roll as u8),
                1 => out.extend(std::iter::repeat_n(
                    (roll >> 8) as u8,
                    (roll >> 16) as usize % 600,
                )),
                _ => out.extend(words[(roll >> 8) as usize % words.len()]),
            }
        }
        out.truncate(size);
        out
    }

    fn assert_both_decoders_read(data: &[u8]) {
        let coded = compress_chunk(data);
        let mut ours = vec![0; data.len()];
        let chunk = zipper::bench::Chunk::Coded {
            packed: &coded,
            size: data.len(),
        };
        zipper::bench::xpress(&[chunk], &mut ours).unwrap();
        assert_eq!(ours, data);
        let mut padded = coded.clone();
        padded.extend([0, 0]);
        assert_eq!(
            xpress_huffman::decompress(&padded, data.len()).unwrap(),
            data
        );
    }

    #[test]
    fn chunks_of_every_shape_decode_with_zipper_and_with_xpress_huffman() {
        for (seed, size) in [
            (1, 1),
            (2, 2),
            (3, 3),
            (4, 17),
            (5, 300),
            (6, 4096),
            (7, CHUNK),
        ] {
            assert_both_decoders_read(&sample(seed, size));
        }
        assert_both_decoders_read(&[0; CHUNK]);
        assert_both_decoders_read(&(0..CHUNK).map(|at| (at * 7) as u8).collect::<Vec<_>>());
    }

    #[test]
    fn every_code_from_skewed_counts_is_complete_and_at_most_15_bits() {
        let mut frequencies = [0u32; SYMBOLS];
        let mut weight = 1u32;
        for count in frequencies.iter_mut().take(40) {
            *count = weight;
            weight = weight.saturating_mul(2);
        }
        let lengths = code_lengths(&frequencies);
        let space: u64 = lengths
            .iter()
            .filter(|length| **length != 0)
            .map(|&length| 1u64 << (LONGEST_CODE - u32::from(length)))
            .sum();
        assert_eq!(space, 1 << LONGEST_CODE);
        assert!(
            lengths
                .iter()
                .all(|&length| u32::from(length) <= LONGEST_CODE)
        );
    }

    #[test]
    fn the_container_holds_each_chunk_in_order() {
        let data = sample(9, 3 * CHUNK + 5);
        let container = compress(&data);
        let chunks = chunks(&container).unwrap();
        assert_eq!(chunks.len(), 4);
        let inputs = zipper_chunks(&container, &chunks);
        let mut out = vec![0; data.len()];
        zipper::bench::xpress(&inputs, &mut out).unwrap();
        assert_eq!(out, data);
    }
}
