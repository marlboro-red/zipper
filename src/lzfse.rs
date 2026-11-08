use crate::{BadField, Field, field_check, lzvn};

const MAGIC: u32 = 0x0078_7662;
const END: u8 = b'$';
const STORED: u8 = b'-';
const VERSION_1: u8 = b'1';
const VERSION_2: u8 = b'2';
const LZVN: u8 = b'n';

const MATCHES_PER_BLOCK: u32 = 10_000;
const LITERALS_PER_BLOCK: u32 = 4 * MATCHES_PER_BLOCK;
const L_SYMBOLS: usize = 20;
const M_SYMBOLS: usize = 20;
const D_SYMBOLS: usize = 64;
const LITERAL_SYMBOLS: usize = 256;
const SYMBOLS: usize = L_SYMBOLS + M_SYMBOLS + D_SYMBOLS + LITERAL_SYMBOLS;
const L_STATES: u32 = 1 << 6;
const M_STATES: u32 = 1 << 6;
const D_STATES: u32 = 1 << 8;
const LITERAL_STATES: u32 = 1 << 10;
const V2_FIELDS: usize = 24;
const V2_PREFIX: u32 = 8;
const STREAM_PADDING: usize = 8;
const LMD_ZERO_BYTES: usize = 8;

const L_EXTRA: [u8; L_SYMBOLS] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 5, 8];
const M_EXTRA: [u8; M_SYMBOLS] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 5, 8, 11];
const D_EXTRA: [u8; D_SYMBOLS] = [
    0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7,
    8, 8, 8, 8, 9, 9, 9, 9, 10, 10, 10, 10, 11, 11, 11, 11, 12, 12, 12, 12, 13, 13, 13, 13, 14, 14,
    14, 14, 15, 15, 15, 15,
];

// SPEC(Apple lzfse_decode_base.c, lzfse_decode_v1_freq_value): each frequency takes 2 to 14
// bits, read through these two tables indexed by its next five bits.
const FREQUENCY_BITS: [u8; 32] = [
    2, 3, 2, 5, 2, 3, 2, 8, 2, 3, 2, 5, 2, 3, 2, 14, 2, 3, 2, 5, 2, 3, 2, 8, 2, 3, 2, 5, 2, 3, 2,
    14,
];
const FREQUENCY_VALUES: [u8; 32] = [
    0, 2, 1, 4, 0, 3, 1, 8, 0, 2, 1, 5, 0, 3, 1, 24, 0, 2, 1, 6, 0, 3, 1, 8, 0, 2, 1, 7, 0, 3, 1,
    24,
];

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn bits(value: u64, offset: u32, count: u32) -> u32 {
    ((value >> offset) & ((1 << count) - 1)) as u32
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BlockHeader {
    pub(crate) literals: u32,
    pub(crate) literal_bytes: u32,
    pub(crate) matches: u32,
    pub(crate) literal_bits: i32,
    pub(crate) literal_states: [u32; 4],
    pub(crate) lmd_bytes: u32,
    pub(crate) lmd_bits: i32,
    pub(crate) l_state: u32,
    pub(crate) m_state: u32,
    pub(crate) d_state: u32,
    pub(crate) frequency_bytes: u32,
}

pub(crate) fn block_header(fields: &[u8; V2_FIELDS]) -> Field<BlockHeader> {
    let first = u64_at(fields, 0);
    let second = u64_at(fields, 8);
    let header_size = u32::from_le_bytes(fields[16..20].try_into().unwrap());
    let third = u32::from_le_bytes(fields[20..24].try_into().unwrap());
    field_check(
        header_size > V2_PREFIX + V2_FIELDS as u32,
        "LZFSE block header is too short",
    )?;
    let header = BlockHeader {
        literals: bits(first, 0, 20),
        literal_bytes: bits(first, 20, 20),
        matches: bits(first, 40, 20),
        literal_bits: bits(first, 60, 4) as i32 - 7,
        literal_states: [
            bits(second, 0, 10),
            bits(second, 10, 10),
            bits(second, 20, 10),
            bits(second, 30, 10),
        ],
        lmd_bytes: bits(second, 40, 20),
        lmd_bits: bits(second, 60, 4) as i32 - 7,
        l_state: third & 0x3FF,
        m_state: (third >> 10) & 0x3FF,
        d_state: (third >> 20) & 0xFFF,
        frequency_bytes: header_size - V2_PREFIX - V2_FIELDS as u32,
    };
    field_check(
        header.literal_bits <= 0 && header.lmd_bits <= 0,
        "LZFSE block has a bit count out of range",
    )?;
    field_check(
        header.literals <= LITERALS_PER_BLOCK
            && header.literals.is_multiple_of(4)
            && header.matches <= MATCHES_PER_BLOCK,
        "LZFSE block holds too many literals or matches",
    )?;
    field_check(
        header
            .literal_states
            .iter()
            .all(|&state| state < LITERAL_STATES)
            && header.l_state < L_STATES
            && header.m_state < M_STATES
            && header.d_state < D_STATES,
        "LZFSE block starts in a state outside its table",
    )?;
    Ok(header)
}

pub(crate) fn frequencies(coded: &[u8]) -> Field<[u16; SYMBOLS]> {
    let mut frequencies = [0u16; SYMBOLS];
    let mut accumulator = 0u32;
    let mut count = 0u32;
    let mut at = 0usize;
    for frequency in &mut frequencies {
        while count <= 14 && at < coded.len() {
            accumulator |= u32::from(coded[at]) << count;
            count += 8;
            at += 1;
        }
        let index = (accumulator & 31) as usize;
        let width = u32::from(FREQUENCY_BITS[index]);
        field_check(count >= width, "LZFSE frequency table is truncated")?;
        count -= width;
        let mut value = u32::from(FREQUENCY_VALUES[index]);
        if width >= 8 {
            value += (accumulator >> 4) & (0x3FF >> (14 - width));
        }
        accumulator >>= width;
        *frequency = value as u16;
    }
    field_check(
        count < 8 && at == coded.len(),
        "LZFSE frequency table has unused bytes",
    )?;
    Ok(frequencies)
}

fn sums_to(frequencies: &[u16], states: u32) -> bool {
    frequencies.iter().map(|&f| u32::from(f)).sum::<u32>() == states
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    bits: u8,
    extra_bits: u8,
    delta: u16,
    value: u32,
}

// SPEC(Apple lzfse_fse.c, fse_init_value_decoder_table): a symbol of frequency f owns f states.
fn table(states: u32, frequencies: &[u16], extra: impl Fn(usize) -> u8) -> Vec<Entry> {
    let mut table = Vec::with_capacity(states as usize);
    let mut value = 0u32;
    for (symbol, &frequency) in frequencies.iter().enumerate() {
        let extra_bits = extra(symbol);
        let frequency = u32::from(frequency);
        if frequency != 0 {
            let shift = frequency.leading_zeros() - states.leading_zeros();
            let full_width_states = ((2 * states) >> shift) - frequency;
            for index in 0..frequency {
                let (bits, delta) = if index < full_width_states {
                    (shift, ((frequency + index) << shift) - states)
                } else {
                    (shift - 1, (index - full_width_states) << (shift - 1))
                };
                table.push(Entry {
                    bits: (bits + u32::from(extra_bits)) as u8,
                    extra_bits,
                    delta: delta as u16,
                    value,
                });
            }
        }
        value += 1 << extra_bits;
    }
    table
}

// SPEC(Apple lzfse_fse.h, fse_in_checked_init64 and fse_in_checked_flush64): a backward bit
// stream read from its end, whose last byte holds 8 + n bits for n in -7..=0.
struct BitStream {
    bytes: Vec<u8>,
    end: usize,
    accumulator: u64,
    count: u32,
}

impl BitStream {
    fn new(payload: &[u8], unused: i32) -> Field<Self> {
        let mut bytes = vec![0u8; STREAM_PADDING];
        bytes.extend_from_slice(payload);
        let (loaded, count) = if unused == 0 {
            (7, 56)
        } else {
            (8, (64 + unused) as u32)
        };
        let end = bytes.len() - loaded;
        let mut accumulator = 0u64;
        for &byte in bytes[end..].iter().rev() {
            accumulator = accumulator << 8 | u64::from(byte);
        }
        field_check(
            accumulator >> count == 0,
            "LZFSE bit stream has bits set past its end",
        )?;
        Ok(BitStream {
            bytes,
            end,
            accumulator,
            count,
        })
    }

    fn refill(&mut self) -> Field<()> {
        let wanted = (63 - self.count) & !7;
        let bytes = (wanted / 8) as usize;
        field_check(bytes <= self.end, "LZFSE bit stream ends early")?;
        self.end -= bytes;
        for &byte in self.bytes[self.end..self.end + bytes].iter().rev() {
            self.accumulator = self.accumulator << 8 | u64::from(byte);
        }
        self.count += wanted;
        Ok(())
    }

    fn pull(&mut self, count: u32) -> u32 {
        self.count -= count;
        let value = self.accumulator >> self.count;
        self.accumulator &= (1u64 << self.count) - 1;
        value as u32
    }

    fn next(&mut self, state: &mut u32, table: &[Entry]) -> u32 {
        let entry = table[*state as usize];
        let read = self.pull(u32::from(entry.bits));
        *state = u32::from(entry.delta) + (read >> entry.extra_bits);
        entry.value + (read & ((1 << entry.extra_bits) - 1))
    }

    fn unread_payload_bits(&self) -> isize {
        (self.end * 8 + self.count as usize) as isize - (STREAM_PADDING * 8) as isize
    }
}

struct Tables {
    literal: Vec<Entry>,
    l: Vec<Entry>,
    m: Vec<Entry>,
    d: Vec<Entry>,
}

fn tables(coded: &[u8]) -> Field<Tables> {
    let frequencies = frequencies(coded)?;
    let (l, rest) = frequencies.split_at(L_SYMBOLS);
    let (m, rest) = rest.split_at(M_SYMBOLS);
    let (d, literal) = rest.split_at(D_SYMBOLS);
    field_check(
        sums_to(l, L_STATES)
            && sums_to(m, M_STATES)
            && sums_to(d, D_STATES)
            && sums_to(literal, LITERAL_STATES),
        "LZFSE frequency table does not fill its states",
    )?;
    Ok(Tables {
        literal: table(LITERAL_STATES, literal, |_| 0),
        l: table(L_STATES, l, |symbol| L_EXTRA[symbol]),
        m: table(M_STATES, m, |symbol| M_EXTRA[symbol]),
        d: table(D_STATES, d, |symbol| D_EXTRA[symbol]),
    })
}

fn literals(payload: &[u8], header: &BlockHeader, table: &[Entry]) -> Field<Vec<u8>> {
    let mut stream = BitStream::new(payload, header.literal_bits)?;
    let mut literals = vec![0u8; header.literals as usize];
    let mut states = header.literal_states;
    for group in literals.chunks_exact_mut(4) {
        for (index, byte) in group.iter_mut().enumerate() {
            if index % 2 == 0 {
                stream.refill()?;
            }
            *byte = stream.next(&mut states[index], table) as u8;
        }
    }
    field_check(
        stream.unread_payload_bits() == 0,
        "LZFSE literal stream does not end at its start",
    )?;
    Ok(literals)
}

fn decode_v2(
    block: &[u8],
    header: &BlockHeader,
    output: &mut [u8],
    mut written: usize,
) -> Field<usize> {
    let frequency_end = header.frequency_bytes as usize;
    let literal_end = frequency_end + header.literal_bytes as usize;
    let lmd_end = literal_end + header.lmd_bytes as usize;
    let coded = block
        .get(..frequency_end)
        .ok_or(BadField("LZFSE stream ends inside a frequency table"))?;
    let tables = tables(coded)?;
    let literal_payload = block
        .get(frequency_end..literal_end)
        .ok_or(BadField("LZFSE stream ends inside its literals"))?;
    let lmd_payload = block
        .get(literal_end..lmd_end)
        .ok_or(BadField("LZFSE stream ends inside its matches"))?;
    field_check(
        lmd_payload.len() >= LMD_ZERO_BYTES,
        "LZFSE match stream is too short",
    )?;
    let literals = literals(literal_payload, header, &tables.literal)?;
    let mut stream = BitStream::new(lmd_payload, header.lmd_bits)?;
    let (mut l_state, mut m_state, mut d_state) = (header.l_state, header.m_state, header.d_state);
    let mut literal_at = 0usize;
    let mut distance = 0usize;
    for _ in 0..header.matches {
        stream.refill()?;
        let length = stream.next(&mut l_state, &tables.l) as usize;
        stream.refill()?;
        let matched = stream.next(&mut m_state, &tables.m) as usize;
        stream.refill()?;
        let new = stream.next(&mut d_state, &tables.d) as usize;
        if new != 0 {
            distance = new;
        }
        field_check(
            length <= literals.len() - literal_at,
            "LZFSE block uses more literals than it holds",
        )?;
        field_check(
            length <= output.len() - written,
            "LZFSE block decodes past its declared size",
        )?;
        output[written..written + length]
            .copy_from_slice(&literals[literal_at..literal_at + length]);
        written += length;
        literal_at += length;
        if matched != 0 {
            field_check(
                distance != 0 && distance <= written,
                "LZFSE match reaches back before the start of its output",
            )?;
            field_check(
                matched <= output.len() - written,
                "LZFSE block decodes past its declared size",
            )?;
            lzvn::copy_match(output, written, distance, matched);
            written += matched;
        }
    }
    field_check(
        written == output.len(),
        "LZFSE block ends before its declared size",
    )?;
    field_check(
        stream.unread_payload_bits() == (LMD_ZERO_BYTES * 8) as isize
            && lmd_payload[..LMD_ZERO_BYTES] == [0; LMD_ZERO_BYTES],
        "LZFSE match stream does not end at its zero padding",
    )?;
    Ok(lmd_end)
}

fn u32_field(input: &[u8], at: usize) -> Field<u32> {
    let bytes = input
        .get(at..at + 4)
        .ok_or(BadField("LZFSE stream ends inside a block header"))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

pub(crate) fn decode(input: &[u8], output: &mut [u8]) -> Field<()> {
    let mut written = 0usize;
    let mut at = 0usize;
    loop {
        let magic = u32_field(input, at)?;
        field_check(magic & 0xFF_FFFF == MAGIC, "LZFSE block has no magic")?;
        let kind = (magic >> 24) as u8;
        if kind == END {
            at += 4;
            break;
        }
        let size = u32_field(input, at + 4)? as usize;
        field_check(
            size <= output.len() - written,
            "LZFSE stream decodes past its declared size",
        )?;
        let end = written + size;
        match kind {
            STORED => {
                let data = input
                    .get(at + 8..at + 8 + size)
                    .ok_or(BadField("LZFSE stream ends inside a stored block"))?;
                output[written..end].copy_from_slice(data);
                at += 8 + size;
            }
            LZVN => {
                let packed = u32_field(input, at + 8)? as usize;
                let data = input
                    .get(at + 12..at + 12 + packed)
                    .ok_or(BadField("LZFSE stream ends inside an LZVN block"))?;
                lzvn::decode(data, &mut output[..end], written, lzvn::Tail::Exact)?;
                at += 12 + packed;
            }
            VERSION_2 => {
                let fields: &[u8; V2_FIELDS] = input
                    .get(at + 8..at + 8 + V2_FIELDS)
                    .ok_or(BadField("LZFSE stream ends inside a block header"))?
                    .try_into()
                    .unwrap();
                let header = block_header(fields)?;
                let body = at + 8 + V2_FIELDS;
                at = body + decode_v2(&input[body..], &header, &mut output[..end], written)?;
            }
            VERSION_1 => return Err(BadField("LZFSE version 1 blocks are not supported")),
            _ => return Err(BadField("LZFSE block has an unknown kind")),
        }
        written = end;
    }
    field_check(at == input.len(), "LZFSE stream has data after its end")?;
    field_check(
        written == output.len(),
        "LZFSE stream ends before its declared size",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_frequency_reads_back_from_its_shortest_code() {
        for value in 0u32..=1047 {
            let (code, width) = match value {
                0..=1 => ([0b00u32, 0b10][value as usize], 2),
                2..=3 => ([0b001, 0b101][value as usize - 2], 3),
                4..=7 => ([0b00011, 0b01011, 0b10011, 0b11011][value as usize - 4], 5),
                8..=23 => (0b0111 | (value - 8) << 4, 8),
                _ => (0b1111 | (value - 24) << 4, 14),
            };
            let index = (code & 31) as usize;
            assert_eq!(u32::from(FREQUENCY_BITS[index]), width, "{value}");
            let mut decoded = u32::from(FREQUENCY_VALUES[index]);
            if width >= 8 {
                decoded += (code >> 4) & (0x3FF >> (14 - width));
            }
            assert_eq!(decoded, value);
        }
    }

    fn lcg(seed: &mut u64) -> u32 {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (*seed >> 33) as u32
    }

    fn random_frequencies(seed: &mut u64, symbols: usize, states: u32) -> Vec<u16> {
        let mut frequencies = vec![0u16; symbols];
        let used = 1 + lcg(seed) as usize % symbols;
        for _ in 0..states {
            frequencies[lcg(seed) as usize % used] += 1;
        }
        frequencies
    }

    #[test]
    fn every_state_of_every_table_moves_to_a_state_inside_the_table() {
        let mut seed = 7;
        for round in 0..3000 {
            for (states, symbols) in [(L_STATES, 20), (D_STATES, 64), (LITERAL_STATES, 256)] {
                let mut frequencies = random_frequencies(&mut seed, symbols, states);
                if round % 7 == 0 {
                    frequencies.fill(0);
                    frequencies[symbols - 1] = states as u16;
                }
                let table = table(states, &frequencies, |_| 0);
                assert_eq!(table.len(), states as usize);
                for entry in &table {
                    let largest = u32::from(entry.delta) + (1 << entry.bits) - 1;
                    assert!(largest < states, "{states} {frequencies:?}");
                }
            }
        }
    }

    #[test]
    fn stored_and_lzvn_blocks_chain_and_the_stream_ends_exactly() {
        let mut stream = b"bvx-\x03\0\0\0abc".to_vec();
        stream.extend(b"bvxn\x03\0\0\0\x0a\0\0\0\x00\x03\x06\0\0\0\0\0\0\0");
        stream.extend(b"bvx$");
        let mut output = [0u8; 6];
        decode(&stream, &mut output).unwrap();
        assert_eq!(&output, b"abcabc");
        let mut longer = [0u8; 7];
        assert!(decode(&stream, &mut longer).is_err());
        let mut shorter = [0u8; 5];
        assert!(decode(&stream, &mut shorter).is_err());
        let mut extra = stream.clone();
        extra.push(0);
        assert!(decode(&extra, &mut output).is_err());
        for cut in 0..stream.len() {
            assert!(decode(&stream[..cut], &mut output).is_err(), "{cut}");
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) mod apple_tests {
    use super::*;
    use std::{path::Path, process::Command};

    pub(crate) struct Apple {
        _folder: tempfile::TempDir,
        binary: std::path::PathBuf,
    }

    impl Apple {
        pub(crate) fn build() -> Option<Apple> {
            let folder = tempfile::tempdir().unwrap();
            let source = folder.path().join("apple_compression.c");
            std::fs::write(&source, include_str!("../tests/common/apple_compression.c")).unwrap();
            let binary = folder.path().join("apple_compression");
            let built = Command::new("cc")
                .arg("-O2")
                .arg("-o")
                .arg(&binary)
                .arg(&source)
                .arg("-lcompression")
                .status()
                .is_ok_and(|status| status.success());
            assert!(
                built || std::env::var_os("ZIPPER_REQUIRE_APPLE_COMPRESSION").is_none(),
                "cannot build the libcompression helper"
            );
            built.then_some(Apple {
                _folder: folder,
                binary,
            })
        }

        pub(crate) fn run(
            &self,
            operation: &str,
            method: &str,
            input: &[u8],
            capacity: usize,
        ) -> Vec<u8> {
            let folder = tempfile::tempdir().unwrap();
            let (from, to) = (folder.path().join("in"), folder.path().join("out"));
            std::fs::write(&from, input).unwrap();
            let status = Command::new(&self.binary)
                .args([operation, method])
                .arg(&from)
                .arg(&to)
                .arg(capacity.to_string())
                .status()
                .unwrap();
            assert!(
                status.success() || method == "lzvn" && input.len() < 8,
                "{operation} {method}"
            );
            std::fs::read(&to).unwrap()
        }
    }

    pub(crate) fn sample(seed: u64, length: usize) -> Vec<u8> {
        let mut state = seed;
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };
        let mut data = Vec::with_capacity(length);
        while data.len() < length {
            match next() % 4 {
                0 => {
                    for _ in 0..1 + next() % 64 {
                        data.push(next() as u8);
                    }
                }
                1 => {
                    for _ in 0..1 + next() % 300 {
                        data.push(b"the quick brown fox "[next() % 20]);
                    }
                }
                _ if !data.is_empty() => {
                    let back = 1 + next() % data.len().min(300_000);
                    for _ in 0..3 + next() % 400 {
                        data.push(data[data.len() - back]);
                    }
                }
                _ => data.push(0),
            }
        }
        data.truncate(length);
        data
    }

    pub(crate) fn check(apple: &Apple, data: &[u8], label: &str) {
        let capacity = data.len() * 2 + 4096;
        let stream = apple.run("enc", "lzfse", data, capacity);
        assert!(
            data.len() < 100_000 || stream.windows(4).any(|magic| magic == b"bvx2"),
            "{label}: libcompression wrote no LZFSE block"
        );
        let mut ours = vec![0u8; data.len()];
        decode(&stream, &mut ours).unwrap_or_else(|error| panic!("{label}: lzfse {error}"));
        assert!(ours == data, "{label}: lzfse decodes other bytes");
        assert!(
            apple.run("dec", "lzfse", &stream, data.len() + 1) == data,
            "{label}"
        );
        if data.len() < 8 {
            return;
        }
        let raw = apple.run("enc", "lzvn", data, capacity);
        let mut ours = vec![0u8; data.len()];
        let read = lzvn::decode(&raw, &mut ours, 0, lzvn::Tail::Exact)
            .unwrap_or_else(|error| panic!("{label}: lzvn {error}"));
        assert_eq!(read, raw.len(), "{label}");
        assert!(ours == data, "{label}: lzvn decodes other bytes");
    }

    #[test]
    fn apple_streams_of_random_data_decode_as_libcompression_decodes_them() {
        let Some(apple) = Apple::build() else {
            return;
        };
        let mut sizes = vec![1usize, 2, 3, 7, 8, 9, 100, 4095, 4096, 65_536, 65_537];
        sizes.extend([300_000, 1_000_000, 3_000_000]);
        for (seed, &size) in sizes.iter().enumerate() {
            check(&apple, &sample(seed as u64, size), &format!("{size} bytes"));
            let mut noise = sample(seed as u64 + 99, size);
            for (index, byte) in noise.iter_mut().enumerate() {
                *byte ^= (index.wrapping_mul(2654435761) >> 13) as u8;
            }
            check(&apple, &noise, &format!("{size} noisy bytes"));
        }
    }

    #[test]
    fn apple_streams_of_the_silesia_files_decode_as_libcompression_decodes_them() {
        let Some(folder) = std::env::var_os("ZIPPER_SILESIA") else {
            return;
        };
        let apple = Apple::build().expect("ZIPPER_SILESIA needs the libcompression helper");
        let mut checked = 0;
        for entry in std::fs::read_dir(Path::new(&folder)).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                check(
                    &apple,
                    &std::fs::read(&path).unwrap(),
                    &path.display().to_string(),
                );
                checked += 1;
            }
        }
        assert!(checked >= 12, "{checked}");
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn block_headers_never_panic_and_keep_their_counts_inside_the_format() {
        let fields: [u8; V2_FIELDS] = kani::any();
        if let Ok(header) = block_header(&fields) {
            assert!(header.literals <= LITERALS_PER_BLOCK && header.literals % 4 == 0);
            assert!(header.matches <= MATCHES_PER_BLOCK);
            assert!((-7..=0).contains(&header.literal_bits));
            assert!((-7..=0).contains(&header.lmd_bits));
            assert!(header.literal_states.iter().all(|&s| s < LITERAL_STATES));
            assert!(header.l_state < L_STATES && header.m_state < M_STATES);
            assert!(header.d_state < D_STATES);
            assert!(header.frequency_bytes < u32::MAX - V2_PREFIX);
        }
    }
}
