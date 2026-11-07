use crate::shrink::LowBitsFirst;
use std::io::{self, BufRead, Read};

const LONGEST_CODE: usize = 16;
const LITERAL_SYMBOLS: usize = 256;
const LENGTH_SYMBOLS: usize = 64;
const DISTANCE_SYMBOLS: usize = 64;
const LENGTH_WITH_EXTRA_BYTE: u16 = 63;
const WINDOW: usize = 8192;
// SPEC(APPNOTE 4.4.4): general purpose bit 1 selects the 8K window, bit 2 the third tree.
const EIGHT_K_WINDOW: u16 = 1 << 1;
const LITERAL_TREE: u16 = 1 << 2;

type Decoded<T> = Result<T, &'static str>;

fn data_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

// SPEC(APPNOTE 5.3.7): each byte gives a run of up to 16 symbols with one length of 1 to 16 bits.
fn length_run(byte: u8) -> (usize, usize) {
    (usize::from(byte & 0x0F) + 1, usize::from(byte >> 4) + 1)
}

struct Tree {
    counts: [u16; LONGEST_CODE + 1],
    symbols: [u8; LITERAL_SYMBOLS],
}

impl Tree {
    fn from_description(description: &[u8], symbols: usize) -> Decoded<Tree> {
        let mut counts = [0u16; LONGEST_CODE + 1];
        let mut filled = 0;
        for &byte in description {
            let (length, run) = length_run(byte);
            if filled + run > symbols {
                return Err("ZIP Implode tree describes more symbols than it holds");
            }
            counts[length] += run as u16;
            filled += run;
        }
        if filled != symbols {
            return Err("ZIP Implode tree describes fewer symbols than it holds");
        }
        let mut unused = 1i32;
        for &count in &counts[1..] {
            unused = unused * 2 - i32::from(count);
            if unused < 0 {
                return Err("ZIP Implode tree has more codes than fit");
            }
        }
        if unused != 0 {
            return Err("ZIP Implode tree leaves codes unused");
        }
        let mut offsets = [0usize; LONGEST_CODE + 2];
        for length in 1..=LONGEST_CODE {
            offsets[length + 1] = offsets[length] + usize::from(counts[length]);
        }
        let mut sorted = [0u8; LITERAL_SYMBOLS];
        let mut symbol = 0;
        for &byte in description {
            let (length, run) = length_run(byte);
            for _ in 0..run {
                sorted[offsets[length]] = symbol as u8;
                offsets[length] += 1;
                symbol += 1;
            }
        }
        Ok(Tree {
            counts,
            symbols: sorted,
        })
    }

    fn decode<E>(&self, mut bit: impl FnMut() -> Result<u32, E>) -> Result<Option<u8>, E> {
        let (mut code, mut first, mut index) = (0usize, 0usize, 0usize);
        for &count in &self.counts[1..] {
            code |= canonical_huffman_bit(bit()?) as usize;
            let count = usize::from(count);
            if code < first + count {
                return Ok(Some(self.symbols[index + code - first]));
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Ok(None)
    }
}

fn canonical_huffman_bit(shannon_fano_bit: u32) -> u32 {
    shannon_fano_bit ^ 1
}

struct Trees {
    literals: Option<Tree>,
    lengths: Tree,
    distances: Tree,
}

pub(crate) struct Explode<R> {
    bits: LowBitsFirst<R>,
    flags: u16,
    trees: Option<Trees>,
    window: Box<[u8; WINDOW]>,
    position: usize,
    copy_distance: usize,
    copy_left: usize,
    remaining: u64,
}

impl<R: BufRead> Explode<R> {
    pub(crate) fn new(input: R, flags: u16, size: u64) -> Self {
        Explode {
            bits: LowBitsFirst::new(input),
            flags,
            trees: None,
            window: Box::new([0; WINDOW]),
            position: 0,
            copy_distance: 0,
            copy_left: 0,
            remaining: size,
        }
    }

    fn byte(&mut self) -> io::Result<u8> {
        self.bits
            .whole_byte()?
            .ok_or_else(|| data_error("truncated ZIP Implode trees"))
    }

    fn tree(&mut self, symbols: usize) -> io::Result<Tree> {
        let length = usize::from(self.byte()?) + 1;
        let mut description = [0u8; LITERAL_SYMBOLS];
        for byte in &mut description[..length] {
            *byte = self.byte()?;
        }
        Tree::from_description(&description[..length], symbols).map_err(data_error)
    }

    fn start(&mut self) -> io::Result<()> {
        let literals = if self.flags & LITERAL_TREE != 0 {
            Some(self.tree(LITERAL_SYMBOLS)?)
        } else {
            None
        };
        let lengths = self.tree(LENGTH_SYMBOLS)?;
        let distances = self.tree(DISTANCE_SYMBOLS)?;
        self.trees = Some(Trees {
            literals,
            lengths,
            distances,
        });
        Ok(())
    }

    fn symbol(tree: &Tree, bits: &mut LowBitsFirst<R>) -> io::Result<u8> {
        tree.decode(|| Self::take(bits, 1))?
            .ok_or_else(|| data_error("ZIP Implode code is longer than its tree allows"))
    }

    fn take(bits: &mut LowBitsFirst<R>, count: u32) -> io::Result<u32> {
        bits.take(count)?
            .ok_or_else(|| data_error("truncated ZIP Implode stream"))
    }

    fn push(&mut self, byte: u8) {
        self.window[self.position % WINDOW] = byte;
        self.position = self.position.wrapping_add(1);
    }

    fn next_symbol(&mut self) -> io::Result<Option<u8>> {
        let trees = self.trees.as_ref().expect("the trees are read first");
        let bits = &mut self.bits;
        if Self::take(bits, 1)? == 1 {
            let literal = match &trees.literals {
                Some(tree) => Self::symbol(tree, bits)?,
                None => Self::take(bits, 8)? as u8,
            };
            return Ok(Some(literal));
        }
        let low_bits = if self.flags & EIGHT_K_WINDOW != 0 {
            7
        } else {
            6
        };
        let low = Self::take(bits, low_bits)? as usize;
        let high = usize::from(Self::symbol(&trees.distances, bits)?);
        let mut length = u32::from(Self::symbol(&trees.lengths, bits)?);
        if length == u32::from(LENGTH_WITH_EXTRA_BYTE) {
            length += Self::take(bits, 8)?;
        }
        let shortest = if trees.literals.is_some() { 3 } else { 2 };
        let length = u64::from(length + shortest);
        if length > self.remaining {
            return Err(data_error("ZIP Implode match runs past its declared size"));
        }
        self.copy_distance = (high << low_bits | low) + 1;
        self.copy_left = length as usize;
        Ok(None)
    }
}

impl<R: BufRead> Read for Explode<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.trees.is_none() {
            self.start()?;
        }
        let mut written = 0;
        while written < buffer.len() && self.remaining > 0 {
            let byte = if self.copy_left > 0 {
                self.copy_left -= 1;
                self.window[self.position.wrapping_sub(self.copy_distance) % WINDOW]
            } else {
                match self.next_symbol()? {
                    Some(literal) => literal,
                    None => continue,
                }
            };
            self.push(byte);
            buffer[written] = byte;
            written += 1;
            self.remaining -= 1;
        }
        if written == 0 && !buffer.is_empty() && !self.bits.at_end()? {
            return Err(data_error("ZIP Implode stream has data after its end"));
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(tree: &Tree, bits: &[u32]) -> Option<u8> {
        let mut next = bits.iter();
        tree.decode(|| next.next().copied().ok_or(()))
            .ok()
            .flatten()
    }

    #[test]
    fn the_appnote_example_tree_gives_its_codes() {
        let tree = Tree::from_description(&[0x42, 0x01, 0x13], 8).unwrap();
        let expected: [(u8, &[u32]); 8] = [
            (0, &[1, 0, 1]),
            (1, &[1, 0, 0]),
            (2, &[0, 1, 1]),
            (3, &[0, 1, 0]),
            (4, &[0, 0, 1]),
            (5, &[1, 1]),
            (6, &[0, 0, 0, 1]),
            (7, &[0, 0, 0, 0]),
        ];
        for (symbol, bits) in expected {
            assert_eq!(decode_all(&tree, bits), Some(symbol), "{bits:?}");
        }
    }

    #[test]
    fn tree_descriptions_must_fill_their_symbols_with_a_complete_code() {
        assert!(Tree::from_description(&[0xF5, 0xF5, 0xF5, 0xF5], 64).is_ok());
        assert!(Tree::from_description(&[0xF5, 0xF5, 0xF5], 64).is_err());
        assert!(Tree::from_description(&[0xF5, 0xF5, 0xF5, 0xF5, 0x05], 64).is_err());
        assert!(Tree::from_description(&[0xF4, 0xF5, 0xF5, 0xF5], 64).is_err());
        assert!(Tree::from_description(&[0xF6, 0xF5, 0xF5, 0xF5], 64).is_err());
        assert!(Tree::from_description(&[0x00, 0x00], 2).is_ok());
        assert!(Tree::from_description(&[0x00], 1).is_err());
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const PROVEN_DESCRIPTION: usize = 3;

    #[kani::proof]
    #[kani::unwind(17)]
    fn any_short_tree_description_builds_a_decodable_tree_or_is_refused() {
        let description: [u8; PROVEN_DESCRIPTION] = kani::any();
        let length: usize = kani::any();
        kani::assume(length >= 1 && length <= PROVEN_DESCRIPTION);
        let symbols: usize = kani::any();
        kani::assume(symbols >= 1 && symbols <= LITERAL_SYMBOLS);
        let Ok(tree) = Tree::from_description(&description[..length], symbols) else {
            return;
        };
        let bits: u16 = kani::any();
        let mut next = 0;
        let decoded = tree.decode(|| {
            let bit = u32::from(bits >> next & 1);
            next += 1;
            Ok::<u32, ()>(bit)
        });
        let symbol = decoded.unwrap();
        assert!(symbol.is_some_and(|symbol| usize::from(symbol) < symbols));
        kani::cover!(next == 6);
    }
}
