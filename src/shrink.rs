use std::io::{self, BufRead, Read};

const INITIAL_WIDTH: u32 = 9;
const WIDEST: u32 = 13;
const CODES: usize = 1 << WIDEST;
const LITERALS: usize = 256;
const ESCAPE: u16 = 256;
const WIDEN: u16 = 1;
const PARTIAL_CLEAR: u16 = 2;
const FIRST_ENTRY: usize = 257;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Refusal {
    WidePastLargest,
    UnknownControl,
    ControlFirst,
    FullTable,
    Undefined,
    OwnPrefix,
    ClearedPrefix,
}

impl Refusal {
    fn message(self) -> &'static str {
        match self {
            Refusal::WidePastLargest => "ZIP Shrink stream widens its codes past 13 bits",
            Refusal::UnknownControl => "ZIP Shrink stream has an unknown control code",
            Refusal::ControlFirst => "ZIP Shrink stream starts with a control code",
            Refusal::FullTable => "ZIP Shrink stream adds a code to a full table",
            Refusal::Undefined => "ZIP Shrink stream names a code it has not defined",
            Refusal::OwnPrefix => "ZIP Shrink stream has a code whose prefix leads back to itself",
            Refusal::ClearedPrefix => "ZIP Shrink stream has a code whose prefix was cleared",
        }
    }
}

type Decoded<T> = Result<T, Refusal>;

fn data_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) struct LowBitsFirst<R> {
    input: R,
    held: u32,
    count: u32,
}

impl<R: BufRead> LowBitsFirst<R> {
    pub(crate) fn new(input: R) -> Self {
        LowBitsFirst {
            input,
            held: 0,
            count: 0,
        }
    }

    fn byte(&mut self) -> io::Result<Option<u8>> {
        let Some(&byte) = self.input.fill_buf()?.first() else {
            return Ok(None);
        };
        self.input.consume(1);
        Ok(Some(byte))
    }

    fn holds(&mut self, wanted: u32) -> io::Result<bool> {
        while self.count < wanted {
            let Some(byte) = self.byte()? else {
                return Ok(false);
            };
            self.held |= u32::from(byte) << self.count;
            self.count += 8;
        }
        Ok(true)
    }

    pub(crate) fn take(&mut self, wanted: u32) -> io::Result<Option<u32>> {
        if !self.holds(wanted)? {
            return Ok(None);
        }
        let value = self.held & ((1 << wanted) - 1);
        self.held >>= wanted;
        self.count -= wanted;
        Ok(Some(value))
    }

    pub(crate) fn whole_byte(&mut self) -> io::Result<Option<u8>> {
        self.held = 0;
        self.count = 0;
        self.byte()
    }

    fn padding(&self) -> (u32, u32) {
        (self.count, self.held)
    }

    pub(crate) fn at_end(&mut self) -> io::Result<bool> {
        Ok(self.count < 8 && self.input.fill_buf()?.is_empty())
    }
}

enum Step {
    Control,
    Bytes(usize),
}

// SPEC(APPNOTE 5.1): LZW codes of 9 to 13 bits; 256 then 1 widens the code, 256 then 2 frees every leaf, lowest code reused first.
// EXTERNAL(7-Zip ShrinkDecoder): each code reserves the next free entry before it is expanded, so a partial clear can make an entry its own prefix.
struct Table<const N: usize, const W: usize> {
    prefix: [u16; N],
    suffix: [u8; N],
    used: [bool; N],
    string: [u8; N],
    children: [u16; N],
    leaves: [u16; N],
    leaf_at: [u16; N],
    leaf_count: usize,
    free: [u64; W],
    previous: Option<u16>,
    first_byte: u8,
    width: u32,
    escaped: bool,
    #[cfg(test)]
    visited: u64,
}

const NOT_A_LEAF: u16 = u16::MAX;
const FREE_WORDS: usize = CODES / 64;

impl<const N: usize, const W: usize> Table<N, W> {
    fn new() -> Self {
        const { assert!(64 * W >= N && N > FIRST_ENTRY) };
        let mut free = [0u64; W];
        for (word, bits) in free.iter_mut().enumerate() {
            let low = 64 * word;
            let first = FIRST_ENTRY.clamp(low, low + 64) - low;
            let end = N.clamp(first + low, low + 64) - low;
            *bits = ((1u128 << end) - (1u128 << first)) as u64;
        }
        Table {
            prefix: [0; N],
            suffix: [0; N],
            used: [false; N],
            string: [0; N],
            children: [0; N],
            leaves: [0; N],
            leaf_at: [NOT_A_LEAF; N],
            leaf_count: 0,
            free,
            previous: None,
            first_byte: 0,
            width: INITIAL_WIDTH,
            escaped: false,
            #[cfg(test)]
            visited: 0,
        }
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn mid_escape(&self) -> bool {
        self.escaped
    }

    fn string(&self, start: usize) -> &[u8] {
        &self.string[start..]
    }

    fn feed(&mut self, code: u16) -> Decoded<Step> {
        if self.escaped {
            self.escaped = false;
            match code {
                WIDEN if self.width < WIDEST => self.width += 1,
                WIDEN => return Err(Refusal::WidePastLargest),
                PARTIAL_CLEAR => self.partial_clear(),
                _ => return Err(Refusal::UnknownControl),
            }
            return Ok(Step::Control);
        }
        if code == ESCAPE {
            if self.previous.is_none() {
                return Err(Refusal::ControlFirst);
            }
            self.escaped = true;
            return Ok(Step::Control);
        }
        let reserved = match self.previous {
            Some(previous) => Some(self.reserve_entry(previous)?),
            None => None,
        };
        let start = self.expand(code)?;
        if let Some(entry) = reserved {
            self.complete_entry(entry);
        }
        self.previous = Some(code);
        Ok(Step::Bytes(start))
    }

    fn lowest_free(&mut self) -> Option<usize> {
        for word in FIRST_ENTRY / 64..W {
            #[cfg(test)]
            {
                self.visited += 1;
            }
            if self.free[word] != 0 {
                return Some(64 * word + self.free[word].trailing_zeros() as usize);
            }
        }
        None
    }

    fn mark_free(&mut self, entry: usize, free: bool) {
        let bit = 1u64 << (entry % 64);
        if free {
            self.free[entry / 64] |= bit;
        } else {
            self.free[entry / 64] &= !bit;
        }
    }

    fn push_leaf(&mut self, entry: usize) {
        self.leaves[self.leaf_count] = entry as u16;
        self.leaf_at[entry] = self.leaf_count as u16;
        self.leaf_count += 1;
    }

    fn remove_leaf(&mut self, entry: usize) {
        let at = usize::from(self.leaf_at[entry]);
        self.leaf_count -= 1;
        let last = self.leaves[self.leaf_count];
        self.leaves[at] = last;
        self.leaf_at[usize::from(last)] = at as u16;
        self.leaf_at[entry] = NOT_A_LEAF;
    }

    fn reserve_entry(&mut self, previous: u16) -> Decoded<usize> {
        let entry = self.lowest_free().ok_or(Refusal::FullTable)?;
        self.mark_free(entry, false);
        self.used[entry] = true;
        self.prefix[entry] = previous;
        self.suffix[entry] = self.first_byte;
        let parent = usize::from(previous);
        if parent >= FIRST_ENTRY {
            self.children[parent] += 1;
            if self.leaf_at[parent] != NOT_A_LEAF {
                self.remove_leaf(parent);
            }
        }
        if self.children[entry] == 0 {
            self.push_leaf(entry);
        }
        Ok(entry)
    }

    fn complete_entry(&mut self, entry: usize) {
        self.suffix[entry] = self.first_byte;
    }

    fn defined(&self, code: usize) -> bool {
        code < LITERALS || (code < N && self.used[code])
    }

    fn expand(&mut self, code: u16) -> Decoded<usize> {
        let mut walk = usize::from(code);
        if !self.defined(walk) {
            return Err(Refusal::Undefined);
        }
        let mut start = N;
        while walk >= FIRST_ENTRY {
            if start == LITERALS {
                return Err(Refusal::OwnPrefix);
            }
            start -= 1;
            self.string[start] = self.suffix[walk];
            walk = usize::from(self.prefix[walk]);
            if !self.defined(walk) {
                return Err(Refusal::ClearedPrefix);
            }
        }
        start -= 1;
        self.string[start] = walk as u8;
        self.first_byte = walk as u8;
        Ok(start)
    }

    fn partial_clear(&mut self) {
        let leaves = self.leaf_count;
        self.leaf_count = 0;
        for index in 0..leaves {
            #[cfg(test)]
            {
                self.visited += 1;
            }
            let entry = usize::from(self.leaves[index]);
            self.leaf_at[entry] = NOT_A_LEAF;
            self.used[entry] = false;
            self.mark_free(entry, true);
            let parent = usize::from(self.prefix[entry]);
            if parent >= FIRST_ENTRY {
                self.children[parent] -= 1;
                if self.children[parent] == 0 && self.used[parent] {
                    self.push_leaf(parent);
                }
            }
        }
    }
}

pub(crate) struct Unshrink<R> {
    bits: LowBitsFirst<R>,
    table: Box<Table<CODES, FREE_WORDS>>,
    pending: usize,
    remaining: u64,
    finished: bool,
}

impl<R: BufRead> Unshrink<R> {
    pub(crate) fn new(input: R, size: u64) -> Self {
        Unshrink {
            bits: LowBitsFirst::new(input),
            table: Box::new(Table::new()),
            pending: CODES,
            remaining: size,
            finished: false,
        }
    }

    fn next(&mut self) -> io::Result<Option<Step>> {
        let Some(code) = self.bits.take(self.table.width())? else {
            return Ok(None);
        };
        self.table
            .feed(code as u16)
            .map(Some)
            .map_err(|refusal| data_error(refusal.message()))
    }

    fn finish(&mut self) -> io::Result<()> {
        while let Some(step) = self.next()? {
            if let Step::Bytes(_) = step {
                return Err(data_error("ZIP Shrink data goes on past its declared size"));
            }
        }
        if self.table.mid_escape() {
            return Err(data_error("ZIP Shrink stream ends inside a control code"));
        }
        let (count, held) = self.bits.padding();
        if count >= 8 || held != 0 || !self.bits.at_end()? {
            return Err(data_error("ZIP Shrink stream has data after its last code"));
        }
        self.finished = true;
        Ok(())
    }
}

impl<R: BufRead> Read for Unshrink<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        while self.pending == CODES {
            if self.remaining == 0 {
                if !self.finished {
                    self.finish()?;
                }
                return Ok(0);
            }
            match self.next()? {
                None => return Err(data_error("truncated ZIP Shrink stream")),
                Some(Step::Control) => {}
                Some(Step::Bytes(start)) => {
                    let length = (CODES - start) as u64;
                    if length > self.remaining {
                        return Err(data_error("ZIP Shrink data goes on past its declared size"));
                    }
                    self.remaining -= length;
                    self.pending = start;
                }
            }
        }
        let available = self.table.string(self.pending);
        let count = buffer.len().min(available.len());
        buffer[..count].copy_from_slice(&available[..count]);
        self.pending += count;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(codes: &[(u16, u32)]) -> Vec<u8> {
        let (mut out, mut held, mut count) = (Vec::new(), 0u64, 0u32);
        for &(code, width) in codes {
            held |= u64::from(code) << count;
            count += width;
            while count >= 8 {
                out.push(held as u8);
                held >>= 8;
                count -= 8;
            }
        }
        if count > 0 {
            out.push(held as u8);
        }
        out
    }

    fn decode_in_reads_of(packed: &[u8], size: u64, read: usize) -> io::Result<Vec<u8>> {
        let mut decoder = Unshrink::new(packed, size);
        let mut out = Vec::new();
        let mut buffer = vec![0; read];
        loop {
            match decoder.read(&mut buffer)? {
                0 => return Ok(out),
                count => out.extend_from_slice(&buffer[..count]),
            }
        }
    }

    fn refusal_of(stream: &[u16]) -> Option<Refusal> {
        let mut table = Table::<CODES, FREE_WORDS>::new();
        stream.iter().find_map(|&code| table.feed(code).err())
    }

    #[test]
    fn every_read_size_gives_the_same_bytes() {
        let packed = codes(&[
            (97, 9),
            (98, 9),
            (257, 9),
            (259, 9),
            (256, 9),
            (2, 9),
            (99, 9),
            (258, 9),
        ]);
        let whole = decode_in_reads_of(&packed, 11, 64).unwrap();
        assert_eq!(whole, b"abababacccc".to_vec());
        for read in 1..16 {
            assert_eq!(
                decode_in_reads_of(&packed, 11, read).unwrap(),
                whole,
                "{read}"
            );
        }
    }

    #[test]
    fn each_refusal_names_its_cause() {
        let cases: [(&[u16], Refusal); 6] = [
            (&[256], Refusal::ControlFirst),
            (&[97, 256, 3], Refusal::UnknownControl),
            (&[97, 300], Refusal::Undefined),
            (&[97, 98, 257, 256, 2, 99, 257], Refusal::OwnPrefix),
            (&[97, 98, 257, 256, 2, 257], Refusal::OwnPrefix),
            (
                &[97, 98, 99, 100, 259, 256, 2, 101, 257],
                Refusal::ClearedPrefix,
            ),
        ];
        for (stream, refusal) in cases {
            assert_eq!(refusal_of(stream), Some(refusal), "{stream:?}");
        }
        let widest: Vec<u16> = [97].into_iter().chain([256, 1].repeat(5)).collect();
        assert_eq!(refusal_of(&widest), Some(Refusal::WidePastLargest));
        assert_eq!(refusal_of(&[97; 7937]), Some(Refusal::FullTable));
        assert_eq!(refusal_of(&[97; 7936]), None);
    }

    #[test]
    fn table_work_stays_proportional_to_the_codes_under_clear_floods() {
        let mut table = Table::<CODES, FREE_WORDS>::new();
        let mut codes = 0u64;
        let mut feed = |table: &mut Table<CODES, FREE_WORDS>, code: u16| {
            codes += 1;
            table.feed(code).unwrap();
        };
        feed(&mut table, 97);
        let mut next = FIRST_ENTRY as u16;
        while next < 8000 {
            while u32::from(next) >= 1 << table.width() {
                feed(&mut table, ESCAPE);
                feed(&mut table, WIDEN);
            }
            feed(&mut table, next);
            next += 1;
        }
        for _ in 0..20_000 {
            feed(&mut table, ESCAPE);
            feed(&mut table, PARTIAL_CLEAR);
            feed(&mut table, 97);
        }
        assert!(
            table.visited < 200 * codes,
            "{} table slots visited for {codes} codes",
            table.visited
        );
    }

    #[test]
    fn the_declared_size_bounds_the_output_and_the_input() {
        let packed = codes(&[(97, 9), (98, 9), (257, 9)]);
        assert_eq!(decode_in_reads_of(&packed, 4, 8).unwrap(), b"abab");
        assert!(decode_in_reads_of(&packed, 3, 8).is_err());
        assert!(decode_in_reads_of(&packed, 5, 8).is_err());
        let mut longer = packed.clone();
        longer.push(0);
        assert!(decode_in_reads_of(&longer, 4, 8).is_err());
        let control_after = codes(&[(97, 9), (98, 9), (256, 9), (2, 9)]);
        assert_eq!(decode_in_reads_of(&control_after, 2, 8).unwrap(), b"ab");
        let cut_control = codes(&[(97, 9), (98, 9), (256, 9)]);
        assert!(decode_in_reads_of(&cut_control, 2, 8).is_err());
        assert_eq!(decode_in_reads_of(&[], 0, 8).unwrap(), b"");
        assert!(decode_in_reads_of(&[0x61], 0, 8).is_err());
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const PROVEN_ENTRIES: usize = 3;
    const PROVEN_TABLE: usize = FIRST_ENTRY + PROVEN_ENTRIES;
    const PROVEN_WORDS: usize = PROVEN_TABLE.div_ceil(64);
    const PROVEN_CODES: usize = 8;

    type Proven = Table<PROVEN_TABLE, PROVEN_WORDS>;

    fn has_own_prefix(table: &Proven) -> bool {
        (FIRST_ENTRY..PROVEN_TABLE)
            .any(|code| table.used[code] && usize::from(table.prefix[code]) == code)
    }

    fn bookkeeping_matches_the_entries(table: &Proven) -> bool {
        table.leaf_count <= PROVEN_ENTRIES
            && (FIRST_ENTRY..PROVEN_TABLE).all(|entry| {
                let children = (FIRST_ENTRY..PROVEN_TABLE)
                    .filter(|&child| table.used[child] && usize::from(table.prefix[child]) == entry)
                    .count();
                let free = table.free[entry / 64] >> (entry % 64) & 1 == 1;
                let at = usize::from(table.leaf_at[entry]);
                let leaf = table.leaf_at[entry] != NOT_A_LEAF;
                usize::from(table.children[entry]) == children
                    && free != table.used[entry]
                    && leaf == (table.used[entry] && children == 0)
                    && (!leaf || (at < table.leaf_count && usize::from(table.leaves[at]) == entry))
            })
    }

    #[kani::proof]
    #[kani::unwind(9)]
    fn any_short_code_stream_stays_inside_the_table() {
        let mut table = Proven::new();
        assert!(bookkeeping_matches_the_entries(&table));
        let mut cleared = false;
        for _ in 0..PROVEN_CODES {
            let code: u16 = kani::any();
            kani::assume(u32::from(code) < 1 << table.width());
            let escaped = table.mid_escape();
            match table.feed(code) {
                Ok(Step::Bytes(start)) => {
                    assert!(start >= LITERALS - 1 && start < PROVEN_TABLE);
                    kani::cover!(cleared && has_own_prefix(&table));
                }
                Ok(Step::Control) => cleared |= escaped && code == PARTIAL_CLEAR,
                Err(refusal) => {
                    kani::cover!(refusal == Refusal::OwnPrefix);
                    kani::cover!(refusal == Refusal::FullTable);
                    return;
                }
            }
            assert!((INITIAL_WIDTH..=WIDEST).contains(&table.width()));
            assert!(bookkeeping_matches_the_entries(&table));
        }
    }
}
